# RFC 012D: Database-free session-scoped observation

- **Status:** Draft child RFC; proposed observer semantic contract
- **Created:** 2026-08-15
- **Parent:** [RFC 012 umbrella](./012-evidence-backed-adapters-and-progressive-readiness.md)
- **Depends on:** [RFC 012A](./012a-agent-adaptation-and-engine-boundaries.md)
  and [RFC 012C](./012c-runtime-semantics-and-usage-v2.md)
- **Program plan:** [RFC 012 implementation plan](./012-implementation-plan.md)
- **Evidence:** [Phase 0B runtime census](./012-runtime-observation-census-2026-08-15.md)
- **Downstream:** Chopsticks/Godview replacement for `watchSessionTranscript`;
  aggregate-facing reconciliation imports RFC 012C
- **Owns:** scoped observer API semantics, access/scope lifecycle, envelope and
  event identity, transport of common semantic revision references and source
  coverage, contract-version selection, bootstrap/reset/poll ordering,
  backpressure, control lane, scope epochs and per-family resync replacement,
  bounded artifact retrieval, scheduling, and downstream compatibility
- **Does not own:** durable query/readiness authority, adapter decoding,
  runtime fact meanings, or native process lifecycle hooks

## 1. Summary

Spaghetti will provide a non-persistent observer for one explicitly identified
native session and its current/future actor tree. It creates no SQLite database,
does not require the global observation host, and does not enumerate unrelated
agent roots.

The observer is not a second parser. It composes RFC 012A source/scope
declarations, uses the same common source drivers and adapter decoders as the
durable host, and projects RFC 012C facts into typed envelopes with retained
native evidence. The database remains the sole durable/query authority.

Overflow invalidates one scope epoch. Resync builds a complete replacement
snapshot in a new epoch and atomically publishes it at
`observer.resync_complete`. Consumers never merge partial correction replay
into stale state.

## 2. Decisions

1. A scoped observer attaches to one known root identity and follows only
   RFC 012A declared relationships.
2. It opens no Spaghetti store, migration, query pool, durable outbox, or
   configured whole-adapter host.
3. Durable and scoped topologies use the same source identity, generation,
   driver, decoder, fact, and semantic-reducer contracts.
4. Watchers are installed before bootstrap scanning. Notifications remain
   hints; reconciliation is authoritative.
5. Every native-derived envelope has a mandatory deterministic `event_id` and
   every envelope belongs to a `scope_epoch`.
6. Root and actor/run identity travel with every event; team/workflow
   affiliations are orthogonal.
7. Bootstrap, live, correction, reset, and resync are distinct delivery
   semantics.
8. The semantic-event queue and lifecycle/continuity control lane are logically
   separate and bounded. Semantic saturation cannot suppress required control.
9. Queue overflow stops continuity claims and requires explicit full-snapshot
   resync in a new epoch.
10. Artifact content is read through a separate bounded, policy-checked API,
    not embedded as arbitrary stream reads.
11. One slow observer cannot consume another scope's queue budget or starve
    other observers, durable live work, or catalog work.
12. `watchSessionTranscript` remains until the compatibility gates in section
    18 pass.
13. Root session/run identity is final before any watch or event, including an
    attach before native root creation.
14. Observation contract versions are negotiated before source access; unknown
    wire variants are preserved or rejected, never silently dropped.
15. Every typed native-derived semantic event carries its RFC 012A
    `SemanticRevisionRef`; observer `event_id` remains the epoch-delivery
    idempotency key rather than a substitute durable/live join identity.
16. Poll/bootstrap/resync watermarks expose RFC 012A source/family coverage.
    They never imply that observer sequence is comparable to durable commit
    sequence.

## 3. Contract maturity

| Element                                         | Classification         |
| ----------------------------------------------- | ---------------------- |
| Database-free scoped topology                   | Architecture invariant |
| Watch-before-scan/reset-before-replay           | Semantic contract      |
| Event identity and scope-epoch replacement      | Semantic contract      |
| Semantic-revision reference and source coverage | Semantic contract      |
| Dedicated control-lane requirement              | Semantic contract      |
| Actor/scope provenance and raw retention        | Semantic contract      |
| Contract-version selection and unknown variants | Semantic contract      |
| Exact method names and serialized wire structs  | Proposed API           |
| Exact queue sizes and scheduling weights        | Implementation detail  |
| Poll-to-delivery latency                        | Experiment target      |

The semantic contract is ratified before final Rust, N-API, and TypeScript
representations are frozen.

## 4. One decode spine, two sinks

```text
scoped or global source plan
          |
          v
common source driver -> SourceRecord -> adapter decoder -> FactBatch
                                                    |
                         +--------------------------+------------------+
                         |                                             |
                         v                                             v
               DurableProjectionSink                       ObservationProjectionSink
             transaction + SQLite + outbox              bounded in-memory state + envelopes
                         |                                             |
                         v                                             v
              authoritative queries                          runtime side observation
```

Source-control evidence such as create, reset, delete, object error, and
continuity loss comes from the common runtime. Typed semantic events come from
common observation reducers over the same facts used by durable projections.
Adapters do not implement another event parser.

The scoped sink cannot write SQLite, create a persistent cache, publish durable
readiness, or serve as a fallback query store.

## 5. Scope request and access

The semantic request is equivalent to:

```text
ObserveSessionRequest {
  adapter_id
  supported_contract_versions
  root: {
    external_session_ref?: ExternalEntityRef<Session>
    native_session_id?
    declared_fallback_identity_inputs?
    expected_session_key?
    locator
    source_access_grant
  }
  include_descendants: true
  persistence: None
  queue_limits
  raw_record_policy
  artifact_access_policy
}
```

The exact locator is adapter-specific. A Claude SDK convenience may accept
`transcriptPath`; that field does not enter the common source model.

The observer derives the final RFC 012A root `SessionKey` and RFC 012C root
`run_key` from the request and selected support release before installing any
watch. If `external_session_ref` or `expected_session_key` is supplied, its
entity key must match; the external reference's adapter/source/entity kind must
also match the request. Insufficient or mismatched identity fails attachment
with `InvalidRootIdentity`; the observer never emits a provisional key that
later changes.

`expected_session_key` is one concrete RFC 012A base session key, such as the
member selected from an RFC 012B catalog row. A catalog presentation
representative that aggregates several base members cannot be used as the
observer root without selecting one member and locator first.
`external_session_ref` is the persistable form of that same base key; supplying
both fields requires equality.

The request supplies the minimum canonical access root. The observer validates
it and evaluates only RFC 012A `ScopeProgram` primitives. It cannot enumerate
all of `~/.claude`, `~/.codex`, another global source root, or unrelated team,
task, workflow, or project objects to attach one scope.

Every opened object and enumeration is attributed to one declared relation and
recorded in access telemetry.

## 6. Observer API semantics

The semantic surface is equivalent to:

```text
observeSession(request) -> SessionObserver {
  capabilities() -> ObservationCapabilities
  events() -> AsyncIterable<ObservationEnvelope>
  poll() -> Future<ObservationWatermark>
  ready() -> Future<BootstrapBarrier>
  resync(reason?) -> Future<ResyncBarrier>
  readArtifact(request) -> Future<ObservedArtifact>
  close() -> Future<void>
}
```

Watermarks and bootstrap barriers are semantically equivalent to:

```text
ObservationWatermark {
  scope_epoch
  offered_through_sequence
  source_coverage: SourceCoverageSet[]
  scope_coverage
  explicit_object_errors
}

BootstrapBarrier {
  scope_epoch: 1
  barrier_sequence
  source_coverage: SourceCoverageSet[]
  scope_coverage
  explicit_object_errors
  queue_state
  root_present
}
```

`SourceCoveragePoint` and `SourceCoverageSet` have the RFC 012A meaning.
`scope_coverage` is a convenience summary of declared relation/root discovery;
it cannot contradict or replace the sets' scope, membership revision,
completeness, or errors. The exact DTO is provisional, but those semantics and
each point's domain, position, generation, and status cannot be discarded.

The scoped observer reports `Decode` and supported `FactFamily(version)`
coverage only. It cannot claim durable `ProjectionPack` coverage. RFC 012C
aggregate reconciliation compares its fact-family coverage with compatible
fact-family coverage from a durable result and evaluates durable projection
status separately.

Method names and language-specific representation are proposed API; the
following behavior is normative:

- the root may be named before its transcript exists;
- `include_descendants` follows only declared relationships;
- `events()` has one logical consumer;
- `poll()` is an explicit low-latency hint plus reconciliation pass;
- concurrent polls coalesce and unchanged state emits no duplicate semantic
  revision;
- per-object failure is delivered without terminating unaffected objects;
- `close()` is idempotent and waits for owned watches, reads, decodes, and
  deliveries to stop; and
- observation failure cannot fail the native agent process.

Before source access, `observeSession` selects one RFC 012A-compatible model,
external-entity-reference, semantic-revision-reference, coverage, fact-family,
envelope, event, and lifecycle version set. The selected set is reported by
`capabilities()` and every envelope identifies its observation contract
version. If no compatible semantic major exists, attachment fails with
`IncompatibleObservationContract` before opening or watching any object.

Additive minor evolution is allowed only when the consumer can preserve an
unknown event variant with its type tag, bounded encoded value, and envelope
provenance. An unknown wire/event variant is distinct from typed
`unknown_native_evidence`; neither may be silently discarded or reinterpreted.
The variant is represented in an `unknown_wire_event` family and degrades only
the affected capability. A change that is required to reduce an existing
advertised family correctly, changes an existing variant's meaning, or makes a
prior full-snapshot manifest incomplete is a semantic-major change and cannot
use this additive path.
Changing a selected event-family contract version changes delivery `event_id`
values as specified in section 8. It does not change an imported RFC 012A
semantic revision reference unless the underlying fact-family contract itself
changes.

This is Spaghetti client/engine contract negotiation, not native vendor-version
classification. After it succeeds, the first authorized native access may be
RFC 012A's bounded version probe. Scoped typed observation proceeds only after
that probe selects an exact/range-supported release; unverified or incompatible
runtime decoding fails before watches or transcript reads.

## 7. Envelope and identity

Every envelope has semantic equivalents of:

```text
ObservationEnvelope {
  contract_version
  observer_sequence
  scope_epoch
  event_id
  semantic_revision_ref?

  root: {
    session_ref: ExternalEntityRef<Session>
    session_key
    native_session_claim?: NativeIdentityClaim
  }
  actor: ActorRunRef
  actor_attribution: NativeExact | DerivedExact | ScopeFallback {reason}
  affiliations: ActorAffiliationContext

  source: {
    instance_key
    stream_key
    object_key
    locator?
    generation
    record_index?
    cursor_start
    cursor_end
    byte_range?
  }

  native_time?
  observed_at
  phase: Bootstrap | Live | Correction
  evidence: {
    authority
    quality
    effective_at?
    completeness
  }
  event: ObservationEvent
  native_evidence: InlineSourceRecord | Withheld {hash, reason} | EngineControl
}
```

The actor model and typed runtime facts are owned by RFC 012C. Root and actor
identity remain present on controls such as `source.reset`. When a child cannot
be proven for a control or unknown-evidence envelope, the routing actor falls
back to the root with `ScopeFallback`; this is not semantic attribution and
cannot assign a typed fact, usage contribution, task, or message to the root.
A typed family that requires actor identity is delayed, degraded/unavailable,
or retained as unattributed evidence according to RFC 012C rather than guessed.

`semantic_revision_ref` is mandatory when `event` transports a typed
native-derived semantic revision, including a typed retained-unknown fact. It
is absent for observer-only lifecycle controls that have no semantic fact. The
reference is equal to the one returned for the same revision by an
aggregate-facing durable query. An event/control ID cannot be promoted into a
fabricated semantic reference.

`root.session_ref`, `root.session_key`, and any native identity claim refer to
one RFC 012A identity and must agree. The qualified claim is adjacent evidence
and does not participate in reference equality. The reference remains stable
across observer attachments and engine restart; the observer does not mint an
attachment-local session handle.

`ActorAffiliationContext` is the RFC 012C reducer context, not another fact.
Late affiliation is delivered as its own semantic revision. The context on an
unchanged event is not part of that event's identity and cannot cause same-epoch
redelivery; a new replacement epoch may replay the stable event ID with the
current context because deduplication is epoch-scoped.

Envelope-level `evidence` qualifies the existence/timing of the emitted
revision as a whole. Field-level RFC 012A `QualifiedValue`s remain authoritative
for their values; envelope quality cannot promote, average, or replace them.

`observer_sequence` is delivery order within one attachment and is not stable
across attachments. Record order is preserved within one source
object/generation. Across objects, sequence is delivery order only; timestamps
are retained without fabricating a stronger causal order.

## 8. Deterministic event IDs

`event_id` is mandatory.

For a typed semantic revision it is derived from:

```text
event contract version
event kind
SemanticRevisionRef
source record/declared correction occurrence reference
deterministic within-record semantic subkey when needed
```

The occurrence component is mandatory because a replaceable entity may
legitimately transition `A -> B -> A`: both `A` values share one durable/live
join identity, but the second `A` is a new ordered delivery that must not be
discarded as a duplicate. An immediately repeated `A` with no intervening
accepted revision is suppressed by the semantic reducer before event
construction. The occurrence reference is the RFC 012A source-record identity
for native evidence, or an equally deterministic declared correction/barrier
identity for engine-derived corrections; it is never observer sequence or wall
clock.

When no semantic revision key exists, it is derived from:

```text
adapter ID
source instance/object/generation
cursor range or document revision
event kind
deterministic within-record semantic subkey or emission ordinal
```

The ordinal is part of the decoder contract and cannot depend on hash-map
iteration, thread timing, or scheduling. An ID cannot include
`observer_sequence`, `observed_at`, delivery phase, or `scope_epoch`; unchanged
accepted native occurrences retain the same ID across replay.

`event_id` is the idempotency key for the selected observer event contract.
Cross-topology durable/live reconciliation uses `SemanticRevisionRef`, not
`event_id`, because a compatible observer event representation may evolve
independently from a durable query representation.

Engine-control IDs derive from scope identity, epoch, control kind, and the
relevant source/barrier identity. Consumers deduplicate delivery by
`(scope_epoch, event_id)`, not by `event_id` across epochs, because a new epoch
must rebuild a fresh replacement state.

## 9. Scope expansion and actor discovery

The adapter's promoted scope program begins at the known root and may reach:

- existing and future standard child transcripts;
- subagent metadata required for actor type and parent correlation;
- root/session task, todo, and plan objects;
- referenced team configuration, member, and inbox objects;
- workflow run documents, journals, and workflow child transcripts;
- active-session presence matching the root; and
- bounded policy-allowed artifact locators.

For Claude, root, workflow-child, team-member, and standalone-child attribution
are fixture-tested. If a relation cannot be established within declared bounds,
the capability is unavailable or delayed. The engine never broadens to a global
search.

Future descendants are discovered by common membership/watch primitives and
enter the same scope epoch. Team and workflow membership can arrive later than
actor or usage evidence and remains an orthogonal revision.

## 10. Bootstrap and readiness ordering

Attach follows this order:

1. after public contract negotiation, perform the bounded native version probe
   and select the permitted support release;
2. validate root identity, locator, access grant, and scope program;
3. compute the minimum policy-allowed watch anchors;
4. install directory/object watchers before reading current objects;
5. capture the initial native watermark and enumerate only in-scope objects;
6. decode complete records through that watermark as `Bootstrap` in epoch 1,
   retaining partial suffixes;
7. reconcile changes captured during scanning until all objects discovered at
   the barrier watermark are drained;
8. admit `observer.bootstrap_complete` to the ordered multiplexer after all
   prior bootstrap envelopes have been admitted; and
9. resolve engine-level `ready()` with that barrier's sequence, coverage,
   RFC 012A source-coverage sets, explicit object errors, queue state, and
   root presence.

If the root does not yet exist, `ready()` may resolve with
`root_present: false` after its watch anchor is active. Later creation is
`Live`, not reset or correction.

Delivery terms are normative:

- **admitted/offered** means accepted into the bounded ordered multiplexer;
- **delivered** means the async iterator or transport has yielded the envelope
  to the consumer; and
- **applied** means consumer-owned reduction has processed it.

Engine-level `ready()` proves admission through `barrier_sequence`; it does not
prove consumer application. A consumer-ready SDK helper owns the one logical
event drain, applies every envelope through that sequence, and only then resolves
with the reduced bootstrap state.

The helper maintains an attachment-local application receipt for each yielded
envelope. A receipt binds the attachment authority, `scope_epoch`,
`observer_sequence`, and `event_id`; it is flow-control evidence, not a semantic
identity, durable cursor, or portable deduplication key. The helper acknowledges
the receipt only after its consumer-owned reducer successfully applies the
envelope. Acknowledging the latest successful receipt is idempotent. A receipt
from another attachment, a receipt that does not match the pending envelope, or
an attempt to skip a yielded envelope advances no applied state.

The initial helper permits one application-pending envelope. It does not dequeue
a later envelope until the pending one is acknowledged or the helper is
cancelled. This bounds post-delivery state and also ensures that a mandatory
control can be delayed only by the one envelope already yielded to the consumer,
as allowed by section 12. An implementation may later pipeline a bounded number
of receipts only if it preserves the same ordered acknowledgement proof and
cannot let delivery outrun bounded application state.

Acknowledging `observer.bootstrap_complete` establishes consumer bootstrap
readiness; acknowledging `observer.resync_complete` establishes the applied
replacement barrier. Because acknowledgements follow the single yielded order,
either acknowledgement proves application of every earlier yielded envelope in
that lifecycle. Explicit sequence gaps caused by a preceding
`observer.resync_required` are valid: superseded envelopes were never yielded and
therefore require no fabricated acknowledgement. Application failure leaves the
exact receipt pending and consumer readiness unresolved; it does not cause the
engine to claim application or manufacture continuity loss.

The supported consumption order is:

```text
observer = observeSession(request)
drain = consume_concurrently(observer.events(), apply_envelope)
bootstrap = await observer.ready()
```

`ready()` observes the barrier; it does not start delivery. During bootstrap or
correction, a full semantic queue applies bounded producer backpressure:
scanning pauses, `ready()` remains pending, and reconciliation catches up after
draining. Queue fullness alone cannot manufacture continuity loss. Awaiting
`ready()` without driving the event stream may remain pending but cannot force
unbounded buffering or artificial overflow. The supported SDK helper starts the
drain before exposing consumer-ready state. Applications must use either the
concurrent-drain pattern above or that helper; awaiting engine `ready()` and only
then starting the sole event drain is not a supported ordering.

## 11. Partial records, reset, and polling

For append-delimited objects:

- a partial logical record remains buffered until complete;
- byte offset alone cannot prove file continuity;
- truncation, replacement, native file-identity change, or declared
  discontinuity increments generation;
- `source.reset {old_generation, new_generation, reason}` is delivered before
  any corrected replay;
- replay after true reset is `Correction`; and
- cold records from an unchanged newly attached source are `Bootstrap` and do
  not fabricate reset/rewrite.

`poll()` returns only after every complete change visible to its watermark has
been decoded and offered to the semantic queue, or its result explicitly
reports object/continuity state preventing that offer. Polls are serialized or
coalesced and idempotent at the semantic-revision level. Its source coverage is
the coverage actually offered through `offered_through_sequence`; it cannot
advance merely because a watcher observed a newer file timestamp.

Object deletion and reset retract old object/generation-owned semantic state
through common reducers. Per-object errors carry provenance and retry state and
do not kill sibling observation.

## 12. Bounded queues and dedicated control lane

Each observer owns:

1. a semantic-event queue bounded by event count and retained-native byte
   budget; and
2. a separate small lifecycle/continuity control lane.

The two lanes are internal scheduling/capacity domains, not two unordered
public streams. The observer multiplexes them into the single `events()` stream
and assigns one monotonic `observer_sequence`. Reset-before-replay,
resync-required-before-new-epoch, and barrier-after-snapshot dependencies are
enforced across both lanes.

The public multiplexer cannot introduce a third FIFO whose semantic backlog can
block mandatory control. When continuity is invalidated, not-yet-delivered
ordinary envelopes from that epoch are removed or bypassed and
`observer.resync_required` becomes the next deliverable envelope, subject only
to an already-yielded envelope. Implementations may use priority selection,
epoch-aware queue invalidation, or an equivalent proof; reserving capacity in a
shared FIFO without proving this property does not conform.

The control lane carries at least:

- source create/reset/delete and terminal object error;
- `observer.resync_required`;
- `observer.resync_started`;
- `observer.resync_complete`;
- observer failure; and
- close/cancellation acknowledgement.

Required continuity/lifecycle controls cannot compete with ordinary semantic
events. Implementations may coalesce controls only when the resulting control
retains the strongest/latest required state and all ordering dependencies. For
example, `resync_required` is sticky until a later epoch begins; it cannot be
coalesced away by an object update.

If a consumer stops draining the public stream, cancellation/close eventually
releases the scope. If the control lane itself cannot preserve a mandatory
lifecycle state, the observer becomes terminally failed and makes no continuity
claim; silent loss is not conforming.

After continuity loss, ordinary semantic delivery stops. Only lifecycle and
continuity controls continue until resync or close. Ordinary events still
queued but not offered in the invalid epoch may be discarded because
`observer.resync_required` explicitly invalidates the entire epoch; this is not
silent continuity loss and the replacement snapshot restores current state.

## 13. Overflow and full-snapshot epoch replacement

Initial bootstrap uses `scope_epoch = 1`. On semantic continuity loss:

1. mark the current epoch invalid;
2. emit `observer.resync_required` through the control lane with invalid epoch,
   last contiguous watermark, and reason;
3. suppress further ordinary delivery in that epoch;
4. on `resync()`, create the next monotonically increasing epoch;
5. emit `observer.resync_started` with old epoch, new epoch, and
   `replacement: FullSnapshot`;
6. install watches first and build a complete supported-scope snapshot with
   phase `Correction` into consumer staging state; and
7. emit `observer.resync_complete` plus a `ResyncBarrier` only after all
   in-scope objects at the new watermark are drained.

The barrier contains:

```text
ResyncBarrier {
  scope_epoch
  replacement: FullSnapshot
  barrier_sequence
  scope_coverage
  family_manifest[] {
    fact_family
    contract_version
    replacement_representation
    completeness
    entity_or_event_count
    semantic_digest
  }
  source_coverage: SourceCoverageSet[]
  explicit_object_errors
  root_present
}
```

`FullSnapshot` means the complete RFC 012C section 12 replacement
representation for every `Supported` or `Degraded` family at the barrier
watermark. Log families replay all unretracted current-generation events;
entity and lifecycle families emit every current reduced entity; usage emits
one latest contribution per response key; unknown evidence follows its declared
bounded representation. The manifest makes absence actionable: an entity is
absent only relative to a complete family/scope entry or an explicit
unavailable/error entry.

The manifest also covers D-owned current state: root presence, in-scope source
object/generation/error state, artifact-availability revisions, and observation
capabilities. Historical lifecycle controls such as an earlier
`resync_required` are not semantic snapshot entities; the new epoch's started
and completion controls establish its lifecycle.

The observer computes `semantic_digest` over semantic IDs, revisions, reduced
payloads, `SemanticRevisionRef` values, quality/completeness, and provenance in
stable order. It excludes epoch, observer sequence, delivery phase, and
observation time. A clean bootstrap and completed resync at the same RFC 012A
coverage vector must have identical family digests.

Consumer rules are normative:

- freeze old-epoch reducer state after `resync_required`;
- stage the new epoch independently;
- deduplicate staging by `(scope_epoch, event_id)`;
- atomically discard all old observer-owned state and idempotency state at
  `observer.resync_complete` and replace it with staging;
- remove every entity absent from the completed replacement snapshot;
- never merge partial staging into the old epoch; and
- discard incomplete staging after failed or re-overflowed resync.

The old epoch may remain visibly displayed as stale during resync, but cannot
receive updates or be treated as current. An unavailable object is explicit in
replacement coverage; facts owned only by that object cannot silently carry
forward. Whole-epoch replacement removes those facts and preserves the explicit
unavailable/error state in the new staging result. A re-overflow uses another
new epoch.

Ordinary live source delete/reset retains explicit retraction semantics within
a valid epoch. Cross-epoch individual tombstones are unnecessary because the
completion barrier replaces the whole observer-owned state.

## 14. Scheduling and observer isolation

Each scope has independent queue, byte, control, epoch, cancellation, and
backpressure state. Shared access/decode pools use starvation-bounded scheduling
across active scopes and durable workloads.

A slow observer may pause, overflow, resync, fail, or close only its own scope.
It cannot consume all global worker permits, prevent another observer's
control delivery, or starve durable live tails/catalog work.

`poll()` promotes already discovered objects in one scope for one bounded pass.
It cannot reprioritize or hydrate the durable database.

## 15. Typed events and capabilities

`ObservationCapabilities` reports `Supported`, `Degraded`, or `Unsupported`
per fact family, with evidence source, quality, expected timing, completeness,
and limitations.

The event union transports RFC 012C semantic revisions for:

- messages and content blocks;
- response usage and qualified model/effort evidence;
- session/permission mode;
- plans, tasks, tools, progress, and compaction;
- actor discovery/activity and affiliation;
- structured user-input requests; and
- unknown native evidence.

It additionally transports RFC 012D source/observer controls and artifact
availability revisions.

The exact authorized native `SourceRecord` is included inline when policy
permits. Withheld evidence includes a hash and reason. Engine controls are
identified as engine evidence and cannot fabricate a native record. A future
reference mode must define lifetime and retrieval before replacing inline
evidence.

## 16. Bounded artifact retrieval

Arbitrary file content does not enter the observation stream. The semantic API
is equivalent to:

```text
ArtifactReadRequest {
  artifact_key
  expected_generation?
  max_bytes
  content_policy
}

ObservedArtifact {
  kind
  locator?
  generation
  provenance
  completeness
  content_hash?
  content?
  unavailable_reason?
}
```

Supported v1 kinds include workflow definitions/native run records, workflow
journals, policy-allowed script locators/content, team configuration, and other
adapter-declared in-scope artifacts.

RFC 012A establishes identity and declared relationships. The common observer
enforces scope, maximum bytes, generation checks, cancellation, and content
policy. Out-of-scope, denied, missing, over-limit, changed-generation,
unsupported, and malformed reads return explicit unavailable reasons; they
cannot widen access or label partial content complete.

## 17. Chopsticks/Godview contract

Chopsticks maps envelopes into state keyed by observer attachment and scope
epoch:

- create a reducer for bootstrap epoch 1;
- process the bootstrap barrier before applying newer hook effects;
- use hooks as root-process lifecycle signals and immediate `poll()` hints;
- consume RFC 012C actor, usage, effective-state, task/plan, interaction, and
  affiliation facts without parsing native payloads;
- preserve each Spaghetti `SemanticRevisionRef` and source-coverage set when
  normalizing or forwarding transcript-derived evidence;
- stage every resync epoch and swap only at `observer.resync_complete`;
- establish/adjust usage baselines for bootstrap/correction rather than
  displaying an instantaneous burn spike; and
- present unavailable evidence honestly.

Transcript evidence is replay/reconciliation authority and covers children.
Hooks may remain the lowest-latency root signal. If hook evidence later enters
Spaghetti, it must reduce to the same semantic identity rather than establish a
parallel model.

## 18. Compatibility and rollout

`watchSessionTranscript` remains supported until all of these pass:

1. attach-before-create, watcher-before-scan, reset, partial-write, poll,
   backpressure, epoch-replacement, artifact, and cancellation conformance;
2. no database creation/call and no unrelated global-root access;
3. equivalent root transcript delivery plus independently tested descendant
   and typed-runtime behavior;
4. stable IDs with no duplicate messages/questions/tasks within an epoch;
5. matching semantic revision references for scoped and durable forms of every
   shared typed family;
6. disappeared entities removed at replacement completion;
7. root, standard-child, workflow-child, and team-member attribution;
8. qualified per-actor usage and correction baseline behavior;
9. multiple simultaneous observers with one slow consumer and no cross-scope
   starvation;
10. feature-flagged Chopsticks migration and rollback; and
11. at least one compatible release retaining the old tail.

Observation failure never changes durable database state or fails the native
agent process.

## 19. Performance status

The following are experiment targets until a gate amendment names the reference
host, fixtures, cache state, repetitions, and accepted variance:

| Metric                                                               | Current target        | Status            |
| -------------------------------------------------------------------- | --------------------- | ----------------- |
| Hook-triggered `poll()` to matching event admission                  | p99 <= 50 ms          | Experiment target |
| Matching admission to iterator/transport delivery with active demand | measured distribution | Experiment target |
| Consumer-ready helper application through barrier                    | measured distribution | Experiment target |
| Queue/retained-native bytes                                          | configured engine max | Semantic ceiling  |
| Unrelated source/database access                                     | exactly zero          | Correctness gate  |
| Close completion                                                     | measured distribution | Experiment target |

Bounded memory, explicit continuity loss, exact access confinement, and
deterministic replacement are correctness gates independent of latency target
calibration.

Caller-requested queue and byte limits are clamped to engine/policy hard maxima;
a request cannot disable boundedness. Delivery latency is measured only while
consumer demand is active and is reported separately from admission so a slow
consumer cannot be misreported as source/decode latency.

## 20. Failure semantics

- Root absence at attach produces an empty complete bootstrap with an active
  watch, not an error or reset.
- Partial records remain buffered until completion or reset.
- One object error is explicit and does not kill siblings.
- Overflow invalidates only the affected epoch and scope.
- Failed/re-overflowed resync cannot publish partial state.
- Observer process crash loses ephemeral state by design; a new attachment
  bootstraps from native evidence.
- Close cancels owned work cleanly and never commits durable state.
- Unknown event families remain bounded evidence rather than terminating the
  stream.

## 21. Rejected alternatives

### 21.1 Require Godview to open the durable host

Rejected because observing one known session should not create SQLite,
configure every root, wait for global discovery, or receive only coarse durable
invalidations.

### 21.2 Keep adapter-private tails

Rejected because each adapter would reimplement watcher order, partial writes,
generation/reset, descendants, backpressure, and raw retention.

### 21.3 One shared event/control queue with a reserved slot

Rejected as the normative abstraction because several lifecycle controls can
coincide. The contract requires a distinct bounded control lane; exact capacity
and coalescing policy remain implementation details.

### 21.4 Merge correction replay into existing consumer state

Rejected because disappeared entities remain stale and replayed entities may
duplicate. Full epoch replacement gives an exact barrier without retaining an
unbounded diff baseline in the observer.

### 21.5 Treat notifications as a lossless event log

Rejected because filesystem notification loss, coalescing, and overflow are
normal. Source reconciliation is authoritative.

## 22. Conformance and acceptance

Release-blocking tests cover:

- compatible observation-version selection, additive unknown wire variants,
  compatible external/semantic-reference and coverage selection, incompatible
  rejection before source access, and stable IDs per selected family version;
- attach before root creation and empty-root bootstrap;
- catalog-provided expected session key agreement, identity mismatch rejection,
  persisted external-session-reference agreement, and final root actor identity
  on pre-creation controls;
- watches installed before scan with simultaneous append;
- existing/future child discovery and bounded scope-access traces;
- root, standalone child, workflow child, and team-member attribution;
- scope-fallback controls/unknown evidence without semantic root attribution;
- two actors and several root observers appending simultaneously;
- partial writes across polls and true reset-before-replay;
- cold bootstrap without fabricated reset;
- deterministic IDs, including multiple same-kind events from one record;
- mandatory semantic revision references on typed native-derived events and no
  fabricated reference on observer-only controls;
- equality with durable-query semantic references for every shared family;
- evolving usage for one `usage_key` producing distinct deterministic
  revision/event IDs while an exact repeated current revision is suppressed;
- an `A -> B -> A` usage reversion carrying equal semantic references for both
  `A` values but distinct deterministic occurrence event IDs within one epoch;
- idempotent/coalesced polling and unknown-event retention;
- bootstrap larger than the semantic queue with the SDK helper's internal drain
  active while the caller awaits consumer-ready state, then safe completion
  without artificial overflow;
- engine `ready()` without an active drain resolving only when bounded admission
  fits, otherwise remaining bounded/pending rather than deadlocking workers,
  overflowing, or accumulating unbounded state;
- dedicated control delivery under semantic saturation;
- disappearance during lost continuity and atomic replacement removal;
- per-family replacement manifests/digests and explicit incomplete/unavailable
  coverage without stale carry-forward;
- poll/bootstrap/resync source-coverage parity with common driver semantics,
  including scope-membership additions/removals/omissions and incomparable
  generation/document positions;
- unchanged message/question/task replay without duplicates in the new epoch;
- failed and re-overflowed resync without partial swap;
- at least three active root observers, one slow enough to overflow, while the
  others retain continuity and latency;
- per-object error isolation and cancellation at every boundary;
- exact zero database operations and unrelated-root enumeration;
- typed RFC 012C runtime facts and usage baseline behavior;
- bounded artifact allow/deny/missing/generation/size cases; and
- close at attach/read/decode/delivery/resync boundaries.

RFC 012D is complete when the scoped observer passes the full matrix, access
traces prove confinement, clean-bootstrap and resync replacement-state digests
match per RFC 012C family at the same RFC 012A coverage vector, slow scopes
remain isolated, Chopsticks passes its feature-flagged downstream suite, and
compatibility rollback remains available.
