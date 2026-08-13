# RFC 011 Phase 9: delegation and workflow query pack

Status: Phase 9 slice complete; Phase 10 production cutover completed on 2026-08-12

> Historical boundary: shadow/legacy ownership language below records the
> state when this query pack landed. The current Rust-owned topology and
> remaining rollout evidence are in the
> [Phase 10 closure ledger](./011-phase-10-closure.md).

This slice exposes current child-run delegation relations, workflow
containers, workflow details, and workflow-member journal evidence through
the persistent Rust query pool. It completes the last named production-query
capability in the RFC 011 Phase 9 port list. The surface remains staged on the
isolated Claude observation shadow; it does not by itself satisfy the Phase 9
benchmark/conformance exit gate or perform the Phase 10 client cutover.

## Query contract

The engine and SDK expose four asynchronous operations:

```text
listDelegations({
  projectId, sessionId,
  workflowId?, standaloneOnly?, cursor?, limit?
})
listWorkflows({ projectId, sessionId, cursor?, limit? })
getWorkflow(workflowId)
listWorkflowMembers({ workflowId, cursor?, limit? })
```

Project/session membership is verified from opaque canonical identities.
`workflowId` scopes delegation discovery to child runs named by that
workflow's current member projection. `standaloneOnly` excludes every child
run that is a current member of any workflow. The two filters are mutually
exclusive, and a scoped workflow must belong to the requested session.

Every operation reports contract version `1` and `atCommitSeq` from one
SQLite read transaction on a read-only/query-only worker. List operations
default to 50 rows and reject limits outside 1 through 200. Their opaque,
versioned keyset cursors bind the query kind, complete scope, position, and
first page's commit watermark. A malformed, cross-query, cross-scope, or
expired cursor is rejected rather than reinterpreted.

Delegations order qualified source time newest first, place untimed evidence
after timed evidence, and break ties by stable child-run identity. Workflows
use finish time, then start time, with the same timed-before-untimed rule and
stable workflow identity. Workflow members order by native agent ID and
stable member identity. Dedicated additive indexes mirror all three keyset
orders.

## Delegation semantics

One delegation row is the current canonical child relation, enriched only by
explicitly joined projections. It includes:

- child and optional parent run identity plus verified project/session scope;
- native child/task/run identifiers and current native metadata;
- relation kind, evidence strength, resolution state, and presence flags;
- exact native-spawn fields and anchor message when the decisive relation has
  an explicit spawn assertion;
- observed child run state, message count, and workflow-member count;
- every decisive fact identity, assertion/conflict counts, and source
  provenance.

An explicit native relation uses the decisive spawn fact for its source
observation provenance; layout relations use their decisive relation fact.
The spawn anchor is joined by the exact decisive spawn and canonical message
identity. The query never correlates by path, callback order, label text, or a
loose child/task search. Unresolved and conflicting relations remain visible.

## Workflow and member semantics

`listWorkflows` returns both snapshot-backed workflows and journal-only
containers. Snapshot state, workflow resolution, native status, normalized
container status, member-event counts, unresolved/conflicting member counts,
native `agentCount` comparison, and join conflicts remain separate fields.
For a journal-only row, one deterministic member fact supplies provenance;
missing summary fields remain absent.

`getWorkflow` returns the same summary together with the workflow's model,
script, arguments, summary/error, and complete native snapshot. Rust enforces
a 16 MiB bound over the native JSON and returned detail strings. The snapshot
is preserved for forward-compatible inspection; clients do not need to
reconstruct it from flattened columns.

`listWorkflowMembers` returns the durable `started`, `result_observed`, or
`orphan_result` reduction for each native member. A deterministic child-run
identity is always available, while `childRunPresent` truthfully reports
whether separate run evidence currently exists. Native result JSON is
returned under a 16 MiB page bound. The member also carries separately joined
run state, delegation state, transcript-message count, decisive event facts,
observation times, and conflict flags.

Workflow completion is not child completion. A member result is native
orchestration evidence, not success/failure evidence for the child run. The
query does not copy workflow status onto members or infer lifecycle from a
result payload, missing result, journal silence, count difference, or absent
child transcript.

## Compatibility and accepted differences

The TypeScript compatibility service exposes snapshot-backed
`getSessionWorkflows()` rows and filesystem-discovered
`getWorkflowSubagents()` rows. The canonical surface deliberately separates
three concepts that legacy code conflates:

- `canonical_workflows_include_journal_only_containers`: member evidence can
  make an incomplete workflow queryable before its run snapshot arrives;
- `workflow_members_are_event_evidence_not_transcript_inventory`: a journal
  member may exist before or without a child transcript, while a transcript
  relation is queried as a delegation;
- `workflow_results_do_not_imply_child_terminal_state`: native result JSON is
  retained without manufacturing run completion;
- `canonical_membership_count_preserves_native_disagreement`: observed
  members and the snapshot's `agentCount` are reported independently.

The committed Claude shadow fixture compares all common workflow summary
fields with the TypeScript oracle, compares its nested child identifier and
message count, and also asserts the fixture's truthful native count mismatch.

## Schema, cancellation, and conformance evidence

Schema version 33 gains three additive query indexes:

- `idx_canonical_delegations_session_activity` for delegation chronology;
- `idx_canonical_workflows_session_activity` for scoped workflow chronology;
- `idx_canonical_workflow_members_workflow_order` for member paging.

The existing two-column member index remains unchanged so same-version
databases are not left with an obsolete definition. Rust and transitional
TypeScript schema authorities carry identical DDL. Same-version startup
reruns `CREATE INDEX IF NOT EXISTS`, so the additive indexes do not require a
projection rebuild. Planner tests require SQLite to use each ordering index.

Rust fixtures cover workflow and standalone delegation scopes, metadata and
provenance, snapshot detail, member results, missing child runs, stable
multi-page member walks, cursor kind/scope/watermark rejection, invalid
limits and identity membership, query purity, and pre-queue cancellation.
All work also inherits the common cancellation epoch and SQLite progress
handler for in-flight interruption.

The SDK shadow test exercises all four operations through generated N-API
declarations, checks row and payload contracts, compares the normalized
legacy oracle, rejects incompatible delegation filters, and verifies a
pre-aborted workflow request.

## Remaining Phase 9 and cutover work

Every named production-query category in the Phase 9 port list now has a Rust
shadow implementation. The consolidated N-API conformance, scaled-history,
payload-boundary, cancellation, and concurrent-refresh evidence is recorded
in the [Phase 9 query gate](./011-phase-9-query-conformance-benchmark.md).

The remaining work is shared IPC/domain DTO generation, benchmarking the
selected IPC topology, operational scale-50/private-corpus soak evidence, and
Phase 10 production-client migration plus retirement of TypeScript SQLite
query ownership.

Until those gates pass, the legacy TypeScript query service remains the
production read owner and the Rust observation database remains isolated.
