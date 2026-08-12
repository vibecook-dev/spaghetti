# RFC 011 Phase 5: Claude workflow capability pack

Status: implemented on 2026-08-11

This slice moves Claude's workflow run summaries and append journals into the
common Rust observation engine. It makes workflow containers and member-event
evidence durable without converting orchestration completion, missing results,
or filesystem silence into child-run lifecycle claims.

## Corpus and source contract

The reviewed local corpus contained 47 native workflow run documents and 47
matching journals. The journals contained 1,461 records: 753 `started` events
and 708 `result` events. Run status was 44 completed, one failed, and two
killed. The largest run document was 378,172 bytes, the largest journal was
347,779 bytes, and the largest individual journal record was 49,496 bytes.

Claude now declares two canonical streams rooted at `projects`:

- `workflow-runs` selects `*/*/workflows/wf_*.json` and uses
  `ReplaceDocument` with a 1 MiB bound, snapshot-replace consistency, and
  foreground-repair priority;
- `workflow-journals` selects
  `*/*/subagents/workflows/*/journal.jsonl` and uses `AppendDelimitedFile`
  with JSON-line framing, incremental cursor consistency, and interactive
  priority.

Both streams use `MirrorSource` deletion and full raw retention during shadow
migration. Stable reads, append framing, generations, cursor checkpoints,
quarantine, watcher recovery, and scheduling remain common-driver
responsibilities.

The additive streams advance the Claude adapter contract from 8 to 9 and
declare `claude-code-workflow-v1`. Existing stream facts retain their identity
and meaning. Claude declares `runtime.workflows` as native, workflow-granular,
and `EventuallyLive`: journals can append during execution, while the separate
run summary may only become available or settle at the workflow boundary.

## Typed facts and identity

A run path must be exactly
`<project>/<session-uuid>/workflows/<wf_id>.json`. Its payload `runId` must
match the filename. One valid document emits a `WorkflowSnapshotFact` with:

- namespaced workflow, session, and project identity;
- native run/task IDs, workflow name, status, model, script, script path, and
  optional args;
- summary and optional native error;
- exact native start/finish timestamps, duration, agent count, total tokens,
  and total tool calls;
- the complete native JSON object for forward-compatible inspection.

Native `completed`, `failed`, and `killed` statuses normalize only the workflow
container to succeeded, failed, and cancelled. Unknown native statuses remain
queryable as `other` alongside the original status string.

A journal path must be exactly
`<project>/<session-uuid>/subagents/workflows/<wf_id>/journal.jsonl`. A valid
record emits one `WorkflowMemberEventFact` for a non-empty `agentId` and event
`key`. `started` must not carry a result; `result` must carry a result value,
including a native JSON null when explicitly present. Unsupported or malformed
complete records become `UnknownRecord` facts and can advance without being
partially trusted.

Member identity is workflow plus native agent ID. The fact also derives the
common child-run key from the exact native string
`<session>\0<workflow>\0<agent>`, which is the same key used by the nested
subagent transcript adapter. Journal evidence can therefore join before or
after a child transcript without inventing a second run.

## Schema 26 and reduction

Schema 26 adds four provenance-bearing tables:

- `workflow_snapshot_assertions` for independently replaceable run summaries;
- `workflow_member_event_assertions` for append journal evidence;
- `canonical_workflows` for the deterministic query-facing container;
- `canonical_workflow_members` for normalized member observations.

Journal-first arrival creates an incomplete workflow placeholder. A later run
summary resolves it when identity agrees. Summary-first arrival is equally
valid. Canonical workflow rows retain snapshot, competing-value, member,
unresolved-member, and conflicting-member counts. Membership-count status is
explicitly one of `unobserved`, `snapshot_missing`, `matched`, or `different`.

The native `agentCount` matched observed journal membership in 45 of 47
reviewed runs; one differed by minus one and one by plus one. A count difference
is therefore queryable native disagreement, not a conflict by itself.
Competing run documents or cross-half workflow identity disagreement are
conflicts and publish `diagnostic.runtime.workflow-conflict`.

Member rows reduce to `started`, `result_observed`, or `orphan_result`.
Repeated equivalent assertions retain provenance without manufacturing a
conflict. Competing starts/results, event-key disagreement, or member identity
disagreement remain durable conflicts and publish
`diagnostic.runtime.workflow-member-conflict`. Removing the competing source
clears the corresponding diagnostic in the same transaction. Ordinary changes
publish `runtime.workflow.changed` and `runtime.workflow-member.changed`.

## Replacement and lifecycle non-claims

Each run document revision replaces the summary assertion owned by that source
object, including same-generation edits and confirmed deletion. Journal events
accumulate inside one append generation. Truncation, prefix mismatch, identity
replacement, or contract replay starts a new generation and retracts the old
journal assertions. Canonical foreign keys move before superseded audit facts
are removed.

Workflow completion is not child completion. In the reviewed corpus, completed
workflows still contained 31 started members without result records; the failed
workflow had one and the killed workflows had 13. Completed workflows also
contained member-level error-shaped progress data. Consequently this pack does
not:

- copy workflow status onto a member child run;
- treat a result payload as success or failure evidence for the child;
- infer terminal state from a missing result, journal silence, count mismatch,
  file deletion, or workflow completion;
- add a `canonical_runs`, `run_evidence`, or `observed_run_states` row solely
  from workflow facts.

Child lifecycle remains governed by separately declared transcript, presence,
or native event evidence. This pack only records the workflow container and the
native member events actually observed.

## Conformance evidence

The tests cover:

- declarative selectors, driver bounds, contract/schema versions, capability
  quality, and Rust/TypeScript schema inventory;
- exact run/journal paths, payload/path identity, full run-snapshot retention,
  status normalization, and preserved-unknown behavior;
- exact correlation with the existing nested-subagent child-run key;
- journal-first and summary-first late joins, started/result accumulation,
  explicit orphan results, and native count mismatch;
- same-generation summary replacement, confirmed deletion, journal-generation
  rewrite, canonical retraction, and audit cleanup;
- competing summaries/results, deterministic conflict diagnostics, and
  resolution after source retraction;
- the invariant that a completed workflow with a started-only member creates no
  child run or terminal run-state evidence.

The Rust crate suite contains 372 passing tests after this slice. Repository
type checks, builds, clippy, architecture ratchets, and the Claude small,
Claude medium, Codex, and Grok ingest-differential matrices are the release
gate for the commit.

## Remaining Phase 5 work

Session-index metadata, project memory, and persisted tool results are now
implemented in adjacent Phase 5 packs. Settings and other reviewed sidecar
packs remain. The observation coordinator and production Rust live cutover are
also required for the Phase 5 exit gate.
