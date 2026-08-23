## Status vs landing (2026-08-23)

This petition is the requirements document, written before implementation. It
is unchanged below. This section says what Spaghetti actually ships as of the
RFC 012 landing, so a reader does not mistake a requirement for a delivery.

**Phase A (§7) is met on the Spaghetti side.** The surface, with generated type
names and a working example for each item, is
[docs/integration/vibefield-phase-a.md](../integration/vibefield-phase-a.md).

| §7 Phase A — Spaghetti | State | Shipped as |
| --- | --- | --- |
| Stable project/session refs (§1.1) | shipped | `ExternalEntityRef`, aliased `SessionRef` / `ProjectRef`; `externalRef` on every catalog and history row |
| Native session ID when available | shipped | `CatalogSessionRow.nativeSessionId`; `NativeIdentity` is namespaced, so one id from two products stays two identities |
| Project association evidence (§1.5) | shipped | `associationBasis`, `associationQuality`, `associationProvenance`, and `identityConflicts[]` on each catalog session row |
| Durable query watermark (§1.4) | shipped | `atCommitSeq` on every durable result; `queryWatermark()`, `isSameSnapshot()` |
| Snapshot-consistent pagination (§1.4) | shipped | catalog cursors bound to the watermark page one was answered at |
| Stable semantic event/revision IDs (§1.2) | shipped | `SemanticRevisionRef`, equal across durable queries and the scoped observer; `isSameRevision()` |
| Scoped observer epoch + full-replacement resync (§1.3) | shipped | `scope_epoch`, `overflow`, `resync_complete` with a per-family manifest and digest |

"Stable" in row five means stable *going forward*, and 0.8.0 is the boundary:
revision ids changed for eight runtime families when their identity started
naming the record that proved the value. Entity references did not change. A
consumer that persisted revision ids rebuilds that state once — see caveat 5.

Five caveats a Phase A integrator needs:

1. **The reference shape is not the one §1.1 proposed.** The petition sketched a
   structured `{ adapterId, sourceInstanceId, sessionKey, nativeSessionId }`.
   What shipped is an opaque digest — `ExternalEntityRef` — deriving from
   exactly those inputs but exposing none of them. It satisfies every stated
   requirement (restart-stable, order-independent, namespaced, conflicts
   explicit) and additionally leaks no local path or database identifier.
2. **Identity relations do not exist.** §1.1's "deleted or replaced source
   evidence does not silently cause a different session to reuse the same
   identity" holds — resolution returns `retracted` rather than retargeting.
   But there is no alias, `SameEntity`, or `Supersedes` fact: a project moved
   without provable native identity becomes a new project, and a conflict is
   reported rather than related.
3. **No one joins durable and live for you.** §1.2's cross-topology identity is
   real — the same revision carries the same `SemanticRevisionRef` on both
   sides — but the deduplication is VibeField's to write. Spaghetti ships the
   identity, not the merge.
4. **All eleven fact families are emitted, with typed values** — but two rest on
   narrower evidence than their name suggests. `plan` revisions come from
   `ExitPlanMode`/`EnterPlanMode` tool evidence, not from `plans/<slug>.md`
   sidecars, which stay snapshot facts with no actor binding; and a
   `tool_result` whose call fell outside the bounded correlation window keeps
   content-block evidence without a guessed tool name. Both are recorded in
   [RFC 012C](../rfcs/012c-runtime-semantics-and-usage-v2.md) §7.
5. **Revision ids for eight families changed in 0.8.0.** `message`,
   `content_block`, `tool`, `user_input_request`, `plan`, `task`,
   `native_marker`, and `effective_state` now derive a revision from the record
   that proved the value, because those entities outlive any one record and two
   records can otherwise collapse into a single revision. Entity references —
   the §1.1 surface — are unchanged; only `fact_revision_id` moved, and only for
   those families. Anything VibeField persisted or keyed by those ids must be
   rebuilt once. The rule is
   [RFC 012C](../rfcs/012c-runtime-semantics-and-usage-v2.md) §3.1.

Phases B–D are untouched: no contribution facts, no `code.activity` family, and
no Git or workspace observation exist in Spaghetti, which is what §5 asked for.

Also relevant to anyone consuming token numbers: **usage is now response-level**
and totals are about 2.13× lower than 0.7.x reported, because the old
accounting added every streamed-response repeat. See
[RFC 012C](../rfcs/012c-runtime-semantics-and-usage-v2.md) §6. Grok usage is
exact per response as well, read from `turn_completed` records in
`updates.jsonl` rather than estimated from a session aggregate.

---

## Bottom line

**Yes, but the new requirements are narrower than they first appear.**

The VibeField aggregation layer does **not** require Spaghetti or Chopsticks to absorb product concepts such as aliases, mesh project groups, contribution percentages, or VibeField project IDs.

It does require both engines to expose **stronger integration-grade evidence contracts**:

- **Spaghetti:** stable durable identities, cross-topology event identity, project/session association evidence, watermarks, and optional transcript-derived code-activity evidence.
- **Chopsticks:** an explicit distinction between durable native sessions and process runs, stable runtime event identity, canonical workspace provenance, and Git/workspace finalization records.
- **VibeField:** remains responsible for joining that evidence into projects, sessions, aliases, cross-device groups, and contribution claims.

The current VibeField design already places the live/history merge in `AgentService` and project rollups in `ProjectService`; the UI is not supposed to stitch Spaghetti and Chopsticks independently.

The most important conclusion is:

> **Aggregation mainly hardens identity and reconciliation contracts. Contribution adds repository provenance requirements, primarily on Chopsticks—not Git analytics inside Spaghetti.**

---

# Requirements at a glance

| Capability | Spaghetti | Chopsticks | VibeField |
|---|---|---|---|
| Stable historical session identity | Required/hardened | Expose native session ID | Join into `AgentSessionId` |
| Distinguish session from process run | No runtime ownership | **Required** | Model `AgentSession` + `AgentRun` |
| Project association evidence | Native project/cwd evidence | Workspace/source-root evidence | Decide product project membership |
| Live/history deduplication | Stable semantic IDs + watermark | Preserve source IDs; stable runtime IDs | Perform merge |
| User aliases | No | No | Owns |
| Mesh project grouping | No | No | Owns |
| Workspace baseline/final diff | No | **Required for contribution** | Consume |
| Commit/ref observations | Optional transcript evidence | Required depending on metric | Attribute |
| Contribution claim/confidence | No | No | Owns |
| Historical code attribution | Optional new fact family | Cannot reconstruct past | Owns inference policy |
| Git project/repo analytics | No | No | Repository/Contribution service |

---

# 1. Requirements added to Spaghetti

## 1.1 Stable external references

RFC 012 already requires stable project and session identity and gives every `CatalogSessionFact` a `session_key`, `project_key`, and `native_session_id`. It also requires multiple pieces of catalog evidence to reduce without silently merging identity conflicts.

For VibeField integration, that needs to become an explicit reference contract:

```ts
type SpaghettiSessionRef = {
  adapterId: string;
  sourceInstanceId: string;
  sessionKey: string;
  nativeSessionId?: string;
};

type SpaghettiProjectRef = {
  adapterId: string;
  sourceInstanceId: string;
  projectKey: string;
};
```

The important requirements are:

- References survive process restart.
- They do not depend on pagination order or an in-memory handle.
- `sessionKey` is unique within a defined namespace.
- Native IDs are exposed whenever the adapter can prove them.
- Identity conflicts are returned explicitly.
- Deleted or replaced source evidence does not silently cause a different session to reuse the same identity.

These are mostly **hardening requirements**, not a new Spaghetti domain.

VibeField should treat these as references to **session and project evidence replicas**, not as its own final `AgentSessionId` or `ProjectId`.

---

## 1.2 Cross-topology semantic identity becomes mandatory

This is the largest Spaghetti-side requirement.

RFC 012 currently says:

```text
observer_sequence
event_id  # stable idempotency identity where available
```

and explains that `observer_sequence` is attachment-local while `event_id`, revision keys, generation, and cursor provide idempotency.

For a VibeField live/history merger, **“where available” is not strong enough**.

The same semantic item may arrive through:

```text
Spaghetti scoped observer
        ↓
Chopsticks live projection
        ↓
VibeField AgentService
```

and later through:

```text
Spaghetti durable ingest/query
        ↓
VibeField AgentService
```

VibeField must be able to prove that these are the same item.

Therefore every native-derived semantic revision that can cross this seam needs a deterministic identity:

```ts
type SemanticRevisionRef = {
  eventId: string;
  revisionKey?: string;
  sourceInstanceId: string;
  sourceObjectId: string;
  sourceGeneration: number;
  cursorStart: number;
  cursorEnd: number;
};
```

The same source record reduced through the durable and ephemeral topologies must produce the same semantic identity.

`eventId` must not depend on:

- observer sequence;
- observed time;
- bootstrap/live/correction phase;
- attachment ID;
- delivery order;
- VibeField consumer identity.

A suitable derivation is:

```text
semantic native revision identity, when available
```

or otherwise:

```text
adapter
+ source instance
+ object
+ generation
+ cursor range
+ event kind
+ deterministic semantic subkey
```

Without this law, the aggregate can provide a best-effort timeline, but not a correctness-grade unified timeline.

---

## 1.3 Resynchronization needs epoch/full-replacement semantics

RFC 012 already has explicit bootstrap, overflow, and `resync()` behavior. The observer stops claiming continuity after overflow and must rebuild scoped in-memory state before replaying a correction barrier.

The aggregate layer makes the consumer-side meaning of that replay important.

I would require:

```ts
type ObservationScopeState = {
  scopeEpoch: number;
  barrierSequence: number;
  objects: ObjectWatermark[];
};
```

Rules:

1. Initial attachment begins at `scopeEpoch = 1`.
2. Any resync creates a higher epoch.
3. A resync barrier is a **full replacement of the observer-owned scoped projection**, not an ambiguous stream of possible corrections.
4. VibeField may discard all old ephemeral state for the previous epoch.
5. Durable Spaghetti rows remain independent and are not deleted merely because the ephemeral epoch changed.
6. Events remain idempotent within and across epochs through their deterministic semantic identity.

This is much easier for `AgentService` to consume correctly than trying to infer which old ephemeral rows a correction replay intended to retract.

---

## 1.4 Snapshot-consistent query watermarks

The aggregation layer needs to combine:

```text
durable history through watermark W
+
live observations after W
```

RFC 012 already requires catalog pages to carry a commit watermark and readiness information.

That should be generalized into a stable query contract:

```ts
type DurableHistoryPage = {
  snapshotCommit: number;
  indexedThrough?: number;
  items: TimelineItem[];
  nextCursor?: {
    snapshotCommit: number;
    keyset: unknown;
  };
};
```

Requirements:

- Pagination remains bound to one snapshot.
- A durable invalidation says when a newer complete snapshot exists.
- Timeline queries expose the durable watermark used by the page.
- Session readiness and source coverage are returned with the result.
- A cursor cannot silently continue against a different snapshot.

This lets `AgentService` know exactly where the live overlay begins.

---

## 1.5 Machine-readable project association evidence

Spaghetti’s catalog already links each session to a `project_key`, but VibeField also needs enough evidence to relate that Spaghetti project to:

- a device-local canonical checkout;
- a Chopsticks workspace;
- a VibeField project replica;
- a worktree’s parent project.

Spaghetti should expose source evidence such as:

```ts
type NativeProjectEvidence = {
  projectKey: string;
  nativeProjectKey?: string;

  locator?: {
    kind: "filesystem" | "native-index" | "repository";
    canonicalPath?: string; // local-authorized API only
    nativeValue?: string;
  };

  basis:
    | "native-project-index"
    | "transcript-cwd"
    | "session-directory"
    | "rollout-header"
    | "derived-ancestor";

  quality: "exact" | "native-claimed" | "derived" | "unknown";
  provenance: SourceProvenance;
};
```

Spaghetti does **not** decide that this is VibeField Project A.

It says:

> “This session’s native evidence associates it with this native project/path.”

VibeField’s `ProjectRegistry` decides how that maps to a project replica and project group.

Absolute paths should remain local and policy-bound; mesh summaries should not automatically contain them.

---

## 1.6 Actor identity needs to remain joinable

RFC 012’s observation envelope already carries:

- root session identity;
- mandatory actor `run_key`;
- root/child role;
- parent run key;
- optional native session and native agent IDs;
- team/workflow affiliations.

For project contribution by individual root agent or subagent, Spaghetti must preserve:

```text
actor_run_key
native_agent_id
native_session_id
parent_run_key
tool/message/task ownership
```

across:

- live observation;
- durable history;
- reset/replay;
- usage revisions;
- file/code activity evidence.

VibeField can then aggregate at different levels:

```text
Claude Code provider
→ one durable session
→ root actor
→ individual child actor
```

Spaghetti should not map these to VibeField actor IDs; it should expose the native/evidence-backed identity graph.

---

# 2. Additional Spaghetti requirements specifically for contribution

These requirements depend on the scope of the contribution feature.

## Managed isolated contribution v1: almost no new parser work

For a first version limited to:

- VibeField-managed runs;
- known workspaces;
- dedicated worktrees or exclusive workspaces;
- Git commit and diff metrics;

Spaghetti only needs to provide:

- durable session identity;
- actor identity;
- session/project association;
- transcript readiness.

Git and Chopsticks workspace evidence can provide the actual contribution input.

So contribution v1 does **not** require Spaghetti to become a Git indexer.

## Historical or unmanaged attribution: new evidence facts

To backfill contribution for old sessions or externally launched agents, Spaghetti would benefit from a new optional capability pack:

```text
code.activity
```

Possible facts:

```ts
type FileMutationEvidence = {
  sessionKey: string;
  actorRunKey: string;
  toolCallId?: string;

  path: string;
  operation:
    | "created"
    | "modified"
    | "deleted"
    | "renamed"
    | "unknown";

  nativeTime?: string;
  quality: EvidenceQuality;
  provenance: SourceProvenance;
};

type RepositoryCommandEvidence = {
  sessionKey: string;
  actorRunKey: string;
  toolCallId?: string;

  cwd?: string;
  commandKind:
    | "git-commit"
    | "git-add"
    | "git-reset"
    | "git-rebase"
    | "git-checkout"
    | "git-merge"
    | "other";

  observedCommitOid?: string;
  nativeTime?: string;
  quality: EvidenceQuality;
  provenance: SourceProvenance;
};
```

Important boundary:

> These are transcript-derived evidence records, not contribution claims.

Spaghetti may say:

```text
The transcript contains evidence that actor X invoked a Git commit
and observed commit OID abc123.
```

It must not say:

```text
Claude contributed 38% of Project A.
```

That remains a VibeField conclusion.

## Optional ingestion of Chopsticks own-action records

Chopsticks already plans to persist runtime-owned facts such as workspace finalization, files touched, retained-dirty worktrees, exit classification, and prompt receipts in an append-only own-action record. Its design explicitly says Spaghetti could index that file later if those records become worth searching.

Contribution may be the feature that makes them worth indexing.

A future Spaghetti source adapter could ingest:

```text
Chopsticks own-actions JSONL
```

and produce:

```text
RuntimeRunFact
WorkspaceFinalizationFact
CommitObservationFact
ProcessExitFact
```

This would improve historical reconstruction, but I would classify it as **recommended v2**, not a v1 dependency. ContributionService can initially consume the runtime records directly.

---

# 3. Requirements added to Chopsticks

## 3.1 Separate durable session identity from process-run identity

This is the most important Chopsticks-side semantic requirement.

Chopsticks currently uses both a runtime `sessionId` and a `nativeSessionId`. Its design says it generates the native session UUID at spawn so the runtime session, transcript, and Spaghetti index can join deterministically.

For VibeField, the distinction must be explicit:

```ts
type AgentRunRef = {
  runtimeRunId: string;       // one process lifetime
  providerId: string;
  nativeSessionId?: string;   // durable native conversation
};
```

Rules:

- A new process gets a new `runtimeRunId`.
- Resuming the same native conversation keeps the same `nativeSessionId`.
- One VibeField `AgentSession` may have several `AgentRun`s.
- Process lifecycle events are keyed by `runtimeRunId`.
- Transcript/history joins are keyed primarily by provider plus native session ID.
- A run may temporarily exist before Spaghetti discovers its transcript.
- A historical Spaghetti session may exist with no Chopsticks run.

Chopsticks can continue using the word “session” internally, but the public integration contract must make the lifetime distinction unambiguous.

---

## 3.2 Caller correlation and idempotent creation

VibeField will allocate its own:

```text
AgentRunId
AgentSessionId
ProjectReplicaId
WorkspaceInstanceId
```

Chopsticks should not understand those semantics, but it should permit a generic, bounded host correlation reference:

```ts
type ExternalRef = {
  namespace: string;
  kind: string;
  id: string;
};

type CreateSessionRequest = {
  requestId: string;
  externalRefs?: ExternalRef[];
  // ...
};
```

or permit a caller-provided runtime run ID.

This provides:

- idempotent retry after fieldd uncertainty;
- deterministic recovery after a crash between spawn and metadata persistence;
- correlation for caller-owned terminal adoption;
- no need to infer identity from time or PID later.

Current caller-owned adoption already accepts a caller-supplied `runtimeSessionId` and uses idempotent preparation/adoption behavior, which is a good foundation to generalize.

Chopsticks should treat these references as opaque. It must not interpret `ProjectId` or perform project grouping.

---

## 3.3 Stable runtime event identity and epochs

The current design’s `AgentEventEnvelope` contains:

```text
sequence
sessionId
nativeSessionId
timestamp
source
confidence
event
```

but no stable event ID.

A sequence is useful for one live runtime attachment, but insufficient for:

- fieldd restart;
- mesh reconnect;
- duplicate delivery;
- correction replay;
- resnapshot;
- joining transcript-derived events with durable Spaghetti rows.

Chopsticks should add:

```ts
type RuntimeEventEnvelope = {
  runtimeEpoch: string;
  sequence: number;
  eventId: string;

  runtimeRunId: string;
  nativeSessionId?: string;

  source:
    | "native-hook"
    | "spaghetti-observer"
    | "workspace"
    | "process"
    | "runtime";

  sourceEventRef?: {
    namespace: "spaghetti";
    eventId: string;
    revisionKey?: string;
  };

  correlationIds: {
    turnId?: string;
    messageId?: string;
    toolCallId?: string;
    nativeActorId?: string;
  };

  event: AgentEvent;
};
```

Rules:

- `runtimeEpoch` changes when the runtime host loses in-memory continuity.
- `sequence` orders events within one epoch.
- `eventId` provides idempotency.
- Events originating from Spaghetti preserve the original Spaghetti event identity.
- Corrections target stable event IDs, not only an old sequence number.
- Snapshot responses include the epoch and the latest sequence.
- Subscription after reconnect begins with snapshot/reset, then deltas.

This is a new hard requirement for robust aggregation.

---

## 3.4 Preserve Spaghetti provenance through normalization

Chopsticks currently intends its transcript observer to be a thin wrapper over Spaghetti rather than a second parser, and says hooks remain authoritative for lifecycle while transcripts are authoritative for message content and history. It also requires duplicate messages to be deduplicated against hook events.

The VibeField seam requires one additional rule:

> **Normalization must not erase the original Spaghetti identity and provenance.**

A transcript-derived Chopsticks event should retain:

```text
Spaghetti event ID
revision key
source generation
cursor provenance
actor identity
evidence quality
phase
```

Chopsticks may add runtime context, but it should not mint a completely unrelated event identity.

Otherwise `AgentService` cannot safely replace an ephemeral live row with its later durable Spaghetti equivalent.

---

## 3.5 Stable actor identity

The current Chopsticks normalized event model includes subagent events and recommends correlation through native task/subagent IDs.

For aggregation and contribution, events should carry a common actor shape:

```ts
type RuntimeActorRef = {
  role: "root" | "child";
  nativeActorId?: string;
  nativeSessionId?: string;
  parentNativeActorId?: string;
};
```

Requirements:

- Do not generate a new child identity for each normalized event.
- Preserve provider-native subagent/task IDs.
- Attach actor identity to workspace/process/tool events when known.
- Keep unknown actor attribution explicit.
- Do not silently assign all child work to the root actor.

This is necessary only if VibeField wants to attribute contribution below the session level, but the contract is easier to establish before events are widely consumed.

---

# 4. Chopsticks requirements specifically for contribution

## 4.1 Workspace provenance becomes normative

Chopsticks already models:

- direct;
- exclusive;
- worktree;
- copy;

and its design includes workspace metadata with:

- root;
- source path;
- mode;
- initial commit;
- initial dirty state;
- branch;
- final commit;
- final diff;
- files touched.

That is almost exactly what ContributionService needs.

The change is that contribution makes these fields **a stable, tested product contract**, not optional design detail.

I would formalize:

```ts
type WorkspaceRunProvenance = {
  workspaceInstanceId: string;

  mode: "direct" | "exclusive" | "worktree" | "copy";
  canonicalRoot: string;
  sourceRoot: string;

  repository?: {
    repositoryRoot: string;
    gitCommonDirectory?: string;
    branch?: string;

    headAtStart?: string;
    headAtEnd?: string;

    dirtyAtStart: WorkspaceDiffSummary;
    dirtyAtEnd: WorkspaceDiffSummary;
  };

  ownership: {
    cooperativeIsolation:
      | "dedicated-worktree"
      | "exclusive"
      | "shared"
      | "unknown";

    conflictingManagedRuns: string[];
    externalInterferenceDetected?: boolean;
  };

  filesTouched: string[];
};
```

The runtime does not have to calculate project-level contribution. It only records the run’s workspace facts.

---

## 4.2 Workspace finalization can no longer be casually deferred

The current design says workspace diff and process observers are the first items to defer in later milestones, while Git status polling covers much of their value.

If contribution is a committed near-term feature, this priority changes:

- **Workspace baseline and finalization become required.**
- A full process observer may remain optional.
- Git status/diff checkpoints may be enough for managed worktree v1.
- Finalization must still produce a structured failure if the repository is unavailable or malformed.
- A crash may produce incomplete provenance, but it cannot fabricate a clean final state.

This is probably the largest concrete roadmap addition caused by the contribution feature.

---

## 4.3 Commit/ref observations depend on the metric

There are two different metrics:

### Commits present at run completion

For:

```text
commits reachable from final head but not from initial head
```

the minimum evidence is:

```text
headAtStart
headAtEnd
repository identity
```

ContributionService can inspect the Git graph afterward.

### Every commit created during the run

For:

```text
all commits the run created, including commits later amended,
squashed, reset, or rebased away
```

initial/final heads are insufficient.

Chopsticks then needs durable observations such as:

```ts
type RepositoryObservation =
  | {
      type: "repository.head-changed";
      eventId: string;
      oldOid?: string;
      newOid: string;
      ref?: string;
    }
  | {
      type: "repository.commit-observed";
      eventId: string;
      commitOid: string;
      basis:
        | "owned-process"
        | "workspace-ref-change"
        | "checkpoint-diff";
    }
  | {
      type: "repository.workspace-checkpoint";
      eventId: string;
      headOid?: string;
      indexHash?: string;
      worktreeDiffHash?: string;
    };
```

Recommended v1:

> Count commits accepted/reachable at the selected final ref, not every temporary commit ever created.

That allows contribution v1 without a continuous Git process observer.

---

## 4.4 Isolation and concurrency must be exposed

A diff alone does not prove who made it.

ContributionService needs to know whether the workspace was:

- a dedicated worktree;
- under an exclusive cooperative lease;
- shared with other managed sessions;
- accessible to unknown external processes;
- already dirty before the run.

Chopsticks already acknowledges that exclusive leases are cooperative and do not cover independently launched shell processes.

Therefore Chopsticks must expose isolation as evidence, not imply stronger security than it has.

VibeField might assign:

```text
dedicated worktree + clean baseline + one managed run
    → exact/strong run-to-workspace attribution

shared workspace + multiple active runs
    → weak or unknown attribution
```

The final confidence decision remains in ContributionService.

---

## 4.5 Runtime-owned evidence must be durable

Chopsticks’s own-action record is the correct place to persist facts that cannot be reconstructed from transcripts:

- run created/adopted/resumed;
- workspace allocated;
- baseline captured;
- finalization result;
- commit/ref observations;
- files touched;
- policy conflicts;
- process exit classification.

Its current design already reserves the own-action JSONL for runtime-owned facts rather than duplicating transcript truth.

Contribution adds requirements that those records have:

```text
schema version
stable record ID
runtime run ID
native session ID
workspace instance ID
timestamps
correction/replacement semantics
evidence completeness
```

They must not contain:

- secrets;
- full sensitive environment values;
- arbitrary unbounded file contents;
- raw diffs by default.

Large or sensitive diff content should remain in Git or behind a bounded artifact reference.

---

# 5. Requirements that should not be added to either engine

## Do not add these to Spaghetti

Spaghetti should not own:

- VibeField `ProjectId`;
- project groups across devices;
- project or session aliases;
- user merge/split decisions;
- mesh routing;
- contribution percentages;
- accepted-branch policy;
- Git repository scanning for product analytics;
- human-versus-agent attribution decisions.

Its project/session catalog remains native evidence.

## Do not add these to Chopsticks

Chopsticks should not own:

- historical session browsing;
- project groups;
- project/session aliases;
- repo-family grouping across machines;
- durable contribution dashboards;
- cross-device rollups;
- Git lineage or retained-code analysis;
- VibeField user identity semantics.

It reports runtime and workspace provenance.

## Neither engine needs to become mesh-aware

Each engine remains local to one device/user daemon pair.

`fieldd` adds:

```text
userId
deviceId
peer availability
routing
mesh completeness
```

when projecting local engine facts into the federated product graph.

This preserves the existing VibeField rule that federation adds routing and aggregation rather than new underlying data planes.

---

# 6. Recommended shared integration vocabulary

The systems do not necessarily need one shared code package, but the concepts must align:

```ts
type ProviderId = string;

type NativeSessionClaim = {
  providerId: ProviderId;
  nativeSessionId: string;
};

type SpaghettiSessionRef = {
  adapterId: string;
  sourceInstanceId: string;
  sessionKey: string;
  nativeSessionId?: string;
};

type RuntimeRunRef = {
  runtimeRunId: string;
  runtimeEpoch: string;
  providerId: ProviderId;
  nativeSessionId?: string;
};

type ActorRef = {
  role: "root" | "child";
  nativeActorId?: string;
  parentNativeActorId?: string;
};

type WorkspaceRef = {
  workspaceInstanceId: string;
  canonicalRoot: string;
  sourceRoot: string;
  mode: "direct" | "exclusive" | "worktree" | "copy";
};

type EvidenceDescriptor = {
  authority:
    | "spaghetti"
    | "chopsticks"
    | "git"
    | "vibefield-user";

  quality:
    | "exact"
    | "strong"
    | "weak"
    | "unknown";

  observedAt: string;
  sourceRef: unknown;
};
```

The canonical native-session join is:

```text
(providerId, nativeSessionId)
```

not `nativeSessionId` alone.

The VibeField-owned identities then sit above it:

```text
AgentSessionId
AgentRunId
ProjectId
ProjectReplicaId
ContributionClaimId
```

---

# 7. Minimum requirement sets by phase

## Phase A — basic VibeField aggregation

### Spaghetti

- Stable project/session refs.
- Native session ID when available.
- Project association evidence.
- Durable query watermark.
- Snapshot-consistent pagination.
- Stable semantic event/revision IDs.
- Scoped observer epoch and full-replacement resync.

### Chopsticks

- Explicit runtime-run versus native-session identity.
- Caller correlation/idempotency.
- Stable runtime event ID and runtime epoch.
- Snapshot plus delta subscription.
- Preservation of Spaghetti event provenance.
- Canonical workspace reference.
- Observation capability/quality.

No contribution-specific parser or Git observer is required yet.

---

## Phase B — managed contribution v1

### Spaghetti

No major additional parsing requirement beyond:

- session identity;
- actor identity;
- history readiness.

### Chopsticks

- Workspace mode and canonical root.
- Initial commit and initial dirty state.
- Final commit and final diff.
- Files touched.
- Workspace isolation/conflict state.
- Durable own-action finalization record.

### VibeField

- Git/repository facts provider.
- Contribution claims and confidence.
- Project-level rollups.
- Branch/time/completeness scope.

This is the recommended first implementation.

---

## Phase C — historical and unmanaged contribution

### Spaghetti

- File mutation evidence.
- Git-command evidence.
- Tool-to-file/actor correlation.
- Query by session, actor, path, and time.
- Possibly ingest Chopsticks own-action records.
- Bounded patch/artifact references.

### Chopsticks

No mechanism can recreate runtime facts for sessions that predate it.

For newly observed external sessions, optional additions include:

- process correlation;
- workspace checkpoints;
- commit/ref observations;
- terminal-host adoption provenance.

Historical results must remain confidence-qualified.

---

## Phase D — retained-code and shared-workspace attribution

This adds much more expensive requirements:

- patch identity;
- rename/line lineage;
- squash and cherry-pick reconciliation;
- concurrent writer attribution;
- process/file mutation evidence;
- accepted-branch policy.

Most of that belongs in VibeField’s Repository/Contribution domain, not in Spaghetti or Chopsticks.

---

# 8. The two most important specification changes

I would make these two changes before implementing the aggregate.

## Spaghetti amendment

> Every native-derived observation revision has a deterministic semantic identity shared by durable and scoped topologies. Scoped resynchronization advances a scope epoch and replaces all prior ephemeral state owned by the previous epoch.

## Chopsticks amendment

> A runtime run is a process-lifetime object distinct from the native durable session. Every run exposes stable runtime identity, native-session correlation, event epoch/idempotency, canonical workspace provenance, and structured workspace finalization.

Those two laws eliminate the largest future ambiguity.

---

# Final assessment

The aggregation layer does not require a major expansion of either engine’s responsibility.

For **Spaghetti**, the work is primarily:

```text
make identities, watermarks, project evidence,
and scoped/durable reconciliation stronger
```

For **Chopsticks**, the work is primarily:

```text
make run identity, event identity,
workspace provenance, and finalization stronger
```

The contribution feature then adds one asymmetric requirement:

> **Chopsticks must produce trustworthy workspace/Git provenance for managed runs.**

Spaghetti only needs additional code-activity facts when VibeField later attempts historical or unmanaged attribution.

The clean contract remains:

```text
Spaghetti supplies transcript and historical evidence
Chopsticks supplies runtime and workspace evidence
Git supplies repository truth
VibeField decides identity, grouping, attribution, and metrics
```