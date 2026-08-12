# RFC 011 Phase 5: delegation capability pack

Status: delegation relation, native metadata, and explicit parent-spawn
correlation slices implemented on 2026-08-11

This phase begins with delegation because Claude's Phase 4 transcript streams
already provide stable child identity, session scope, and parent lineage. It
establishes the common capability-pack pattern before teams, presence, tasks,
plans, and artifacts add new snapshot source families.

## Capability contract

Adapter manifests now declare typed capability support rather than opaque
booleans. Each declaration carries a support level, semantic granularity,
temporal availability, and optional notes. Stream declarations reference
validated capability IDs.

Claude declares `runtime.subagents` as:

- `Derived`, because its current parent assertion is interpreted from the
  authoritative transcript layout rather than a native spawn event;
- `Run` granularity, because the subject is a child execution run;
- `Live`, because a delimiter-terminated child transcript record can be
  observed through the shared append driver without waiting for session
  completion.

The declaration explicitly says that silence never proves completion. Claude
does not yet claim terminal lifecycle coverage for this pack.

## Delegation facts

`DelegationFact` is a common, storage-agnostic assertion containing:

- child and optional parent run keys;
- session scope and a truthful delegation kind;
- relation strength (`Layout`, `NativeIndirect`, or `NativeExplicit`);
- optional native child/task IDs, label, prompt, cwd, and worktree path;
- qualified source time.

References are intentionally allowed to be unresolved. A child transcript can
therefore commit before a parent transcript, and neither arrival order nor a
missing sidecar causes the child history to be dropped.

The current Claude transcript decoder emits one layout-strength
vendor-subagent assertion from each subagent transcript record. This is
replay-safe and leaves stronger native parent-spawn correlation for a later
Phase 5 stream.

Because inserting this fact changes the deterministic local fact ordinals of
later subagent facts, the Claude semantic contract is bumped to version 2. A
coordinator encountering a version-1 cursor must start a safe contract replay;
it cannot continue with shifted fact identities.

## Schema 19 and reducer

Schema 19 adds two common projection tables:

- `delegation_assertions` retains every provenance-bearing relation assertion,
  including competing assertions and generation ownership;
- `canonical_delegations` materializes one deterministic current relation per
  child with presence flags, assertion/conflict counts, decisive fact, and one
  of `resolved`, `unresolved_child`, `unresolved_parent`,
  `unresolved_relation`, or `conflicting`.

The reducer ranks explicit native evidence above indirect native evidence and
layout evidence. Disagreement among equally strong assertions is not silently
overwritten: all assertions remain durable, the canonical status becomes
`conflicting`, and the outbox publishes
`diagnostic.runtime.delegation-conflict`. Lower-strength assertions remain
auditable without overriding stronger evidence.

Run and relation changes share the source commit transaction. When a missing
parent arrives later, every delegation referencing that stable run key is
reduced again and `runtime.delegation.changed` is committed with the resolved
state. Generation replacement retracts old assertions before fact ownership is
removed and then reduces the replacement facts atomically.

## Native metadata snapshots and schema 20

Claude now declares `agent-*.meta.json` as a supplemental `ReplaceDocument`
stream with snapshot-replace consistency, run scope, full raw retention, and
a 64 KiB document bound. Its path decoder produces the same child run key as
the sibling transcript, including nested workflow identity.

`DelegationMetadataFact` deliberately contains no parent field or relation
strength. It carries the native child ID, free-form agent type, description,
name, spawn depth, worktree path, and optional spawning tool-use ID. Keeping
metadata separate prevents a native sidecar from accidentally upgrading a
layout-derived parent relation to native evidence.

Schema 20 adds provenance-bearing `delegation_metadata_assertions` and
`canonical_delegation_metadata`. A sidecar can be committed before its run and
remains `unresolved_run`; arrival of the transcript re-reduces it to
`resolved`. Transcript-first arrival resolves in the sidecar commit. Every
new document revision replaces prior assertions owned by the same source
object even inside one generation, and confirmed deletion retracts the
metadata and its audit fact atomically.

Different source objects asserting different native metadata for one child
are retained rather than ordered by callback time. The canonical status is
`conflicting` and the outbox publishes
`diagnostic.runtime.delegation-metadata-conflict`; removal of the competing
snapshot clears the diagnostic.

The additive stream/fact declaration advances the Claude adapter contract to
version 3. Existing transcript fact identities and meanings remain unchanged,
so the coordinator's future per-object decoder-version wiring should avoid an
unnecessary transcript replay for this additive stream.

## Native spawn correlation and schema 21

Claude parent transcripts now emit one `DelegationSpawnFact` for each native
`Task` or `Agent` tool call. The fact owns a parent-scoped spawn key, parent run
and message keys, session, native tool-use ID, tool name, requested label,
prompt, agent type, and qualified source time. It does not claim that a child
exists and does not add lifecycle evidence.

Schema 21 adds provenance-bearing `delegation_spawn_assertions` and
`canonical_delegation_spawns`. The common reducer joins a spawn assertion to a
child metadata assertion only when both expose the same non-empty native
tool-use ID inside the same session. That shared native key produces a
`NativeExplicit` delegation candidate; no callback ordering or text search is
used. The canonical delegation stores both decisive fact IDs, while a direct
layout/native assertion stores its one decisive relation fact. This makes the
join auditable and lets foreign-key retraction remove stale correlations.

Metadata-first and transcript-first arrival converge because both source
halves are persisted before delegation reduction. A sidecar rewrite or
deletion removes the native candidate and falls back to durable layout
evidence in the same commit. Transcript generation replay similarly retracts
old spawn assertions before reducing the replacement. Multiple equally strong
native matches to different parents remain `conflicting` and publish the
existing delegation conflict diagnostic. Spawn rows separately publish
`runtime.delegation-spawn.changed` and
`diagnostic.runtime.delegation-spawn-conflict`.

The new transcript output advances the Claude adapter contract to version 4.
Spawn facts are appended after all previously emitted record facts, so their
addition does not shift existing fact identities. Existing parent transcript
objects still require a targeted contract replay to materialize historical
spawn assertions.

## Conformance evidence

The committed tests prove:

- Claude advertises the pack at the declared quality on parent/subagent
  transcript and metadata streams;
- a decoded subagent record emits layout-strength delegation evidence;
- parent-first arrival resolves in the child commit;
- child-first arrival remains queryable as unresolved and resolves when the
  parent arrives;
- a forced generation replay retracts prior assertions and converges to one
  canonical relation;
- equal-strength conflicting parents remain stored and produce a durable
  conflict diagnostic;
- child activity remains `Active` across late correlation and replay, with no
  completion inferred from silence.
- native metadata uses the identical workflow-aware child key without emitting
  a parent relation;
- sidecar-first and transcript-first arrivals converge, same-generation
  rewrites replace rather than accumulate, and confirmed deletion/recreation
  retracts and restores the canonical row;
- conflicting native sidecars remain durable and publish a conflict diagnostic
  that clears when the competing snapshot is removed.
- native `Task`/`Agent` calls emit parent-scoped spawn facts without emitting
  child or terminal evidence;
- metadata-first and transcript-first native task-ID joins converge to
  explicit lineage with both decisive provenances;
- sidecar deletion and spawn generation replacement atomically fall back to
  layout lineage;
- equal native task IDs from different parent runs remain conflicting rather
  than being ordered by arrival.

The full Rust workspace suite contained 337 passing tests at completion of
these slices; subsequent Phase 5 packs extend that suite.

## Deferred Phase 5 work

This increment does not switch production live observation yet. Legacy
tool-result content that merely mentions an agent ID remains deferred; if
adopted, that heuristic must be represented as `NativeIndirect`, never as the
explicit shared-key relation implemented here. Teams/config/inbox snapshots
active-session presence, tasks/plans, and file-history artifacts are now
implemented in adjacent Phase 5 packs. Other reviewed snapshot packs, the
observation coordinator, and production cutover remain required for the Phase
5 exit gate.
