# RFC 011 Phase 9: capability-detail query pack

Status: Rust capability-detail shadow slice implemented on 2026-08-12

This slice exposes canonical project-memory documents, task collections and
items, global plans, persisted tool-result sidecars, and file-history
artifacts through the persistent Rust query pool. It is staged on the isolated
Claude observation shadow before production client cutover. TypeScript does
not open the shadow database, assemble these rows, or infer relationships that
the committed projections do not contain.

RFC 011 groups these reads under details and optional capability packs. This
record closes that independently testable surface. Canonical FTS, timeline,
and delegation/workflow packs are recorded separately, so this record alone
does not claim the Phase 9 exit gate.

## Query contract

The persistent engine and shadow SDK expose six asynchronous operations:

```text
listMemoryDocuments({ projectId, cursor?, limit? })
listTaskCollections({ sessionId?, runId?, teamId?, cursor?, limit? })
listTasks({ collectionId, cursor?, limit? })
listPlans({ cursor?, limit? })
listToolResults({ projectId, sessionId, cursor?, limit? })
listArtifacts({ sessionId, cursor?, limit? })
```

Every operation executes through one bounded, read-only/query-only Rust
worker, opens one read transaction, returns contract version `1` plus
`atCommitSeq`, and supports request-scoped cancellation through SQLite's
progress handler. Pages default to 50 rows and reject limits outside 1 through
200.

Entity identities and cursors are opaque, typed, and versioned separately for
memory documents, task collections, tasks, plans, tool results, and artifacts.
Each cursor binds the operation, parent/filter scope, order key, and first-page
commit watermark. A cursor expires after a later durable commit rather than
continuing through a drifting snapshot.

`listTaskCollections` accepts at most one of `sessionId`, `runId`, or `teamId`.
An unscoped call is deliberate global discovery, including collections for
which no trusted common owner relation exists. `listToolResults` additionally
verifies that the canonical session belongs to the requested project.

## Canonical semantics and non-claims

Memory documents sort the native index document first and then by stable
native path and identity. They return exact committed content and provenance.
Unlike the legacy `getProjectMemory()` convenience, the page also exposes
topic documents and does not collapse them into `MEMORY.md`.

Task collections expose only relationships persisted by the common
projection. Task items preserve source ordinal, native state, common state,
dependency arrays, resolution evidence, and decisive provenance. The legacy
`.lock`/`.highwatermark` convenience state is not a canonical task fact and is
not recreated.

Plans are global because the plan projection has no trusted session relation.
The query does not reconstruct the legacy `getSessionPlan()` filename
heuristic or manufacture project/session ownership.

Persisted tool results return exact sidecar content plus the canonical
correlation status, matched tool-call/result message identities, match counts,
and explicit join conflict. Artifacts return optional binary content as base64
and the digest as unpadded base64url. Metadata and content can arrive from
different source facts, so their decisive fact IDs and provenance fields stay
separate; an orphan content row never borrows metadata provenance.

## Payload, purity, and performance evidence

Content-bearing pages enforce a 16 MiB response payload limit and report
`payloadBytes` and `payloadByteLimit`. The accounting covers UTF-8 memory,
task, plan, and tool-result content, and the artifact's returned base64
representation. A single row that cannot fit is rejected rather than
truncated. Artifact encoding is binary-safe.

Projection-shaped Rust tests cover ordering, pagination, scope and identity
validation, cursor misuse and expiry, exact content, normalized task state,
tool correlation, two-sided artifact provenance, binary encoding, real
oversized-row rejection, and empty-database query purity. A pre-cancelled
capability request is proven not to enter the worker queue. The Claude shadow
lifecycle test compares memory and persisted tool-result content with the
TypeScript oracle and decodes artifact bytes end to end through N-API.

Additive keyset indexes cover project memory, session/run/team/global task
collection discovery, task items, global plans, persisted tool results, and
session artifacts. Task collection SQL selects a scope-specific predicate so
SQLite can use the corresponding index instead of evaluating optional `OR`
filters over the entire projection. `EXPLAIN QUERY PLAN` tests require the
relevant indexes. DDL is mirrored in Rust and the temporary TypeScript schema
authority without changing schema version 31; same-version initialization
reruns `CREATE INDEX IF NOT EXISTS` statements.

## Remaining Phase 9 work

This is a shadow query surface, not production `SpaghettiClient` cutover. The
remaining gates include:

- timeline/facet and canonical FTS packs are recorded separately;
- delegation/workflow queries are recorded separately;
- large-corpus latency, boundary-size, and concurrent-ingest benchmarks;
- shared IPC/domain DTO generation beyond the current N-API shadow seam;
- production client migration and retirement of TypeScript SQLite query
  ownership in Phase 10.

Until those gates pass, the legacy TypeScript query service remains the
production read owner and the Rust observation database remains isolated.
