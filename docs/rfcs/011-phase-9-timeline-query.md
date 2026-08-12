# RFC 011 Phase 9: canonical timeline, branch, and facet query pack

Status: Rust canonical timeline shadow slice implemented on 2026-08-12

This slice makes a session timeline one coarse Rust query across root and
delegated runs. It returns the page, exact filtered total, unfiltered session
facets, branch metadata, and one committed watermark from the same read
transaction. It is available through the persistent engine and isolated
Claude observation shadow; the production compatibility client is not cut
over yet.

Dedicated delegation/workflow discovery is recorded in its separate Phase 9
slice. This record alone does not claim the Phase 9 exit gate.

## Query contract

The engine and SDK expose:

```text
getTimeline({
  projectId, sessionId,
  roles?, nativeKinds?,
  includeContentKinds?, includeToolNames?,
  excludeContentKinds?, excludeToolNames?,
  search?, branchKind?, cursor?, limit?
})
```

Project/session membership is verified from opaque canonical identities.
`branchKind` is `all` by default and can select `root`, `delegated`, or
`unknown`. An unresolved run is kept visible as `unknown`; it is not silently
classified as root history.

Within each list a value is an exact match. Role and native-kind dimensions
stack with AND semantics. Content-kind and tool-name includes are ORed to
preserve the compatibility solo-filter behavior; exclusions reject a message
when any canonical block matches either excluded dimension. Content kinds are
the versioned common set `text`, `thinking`, `tool_call`, `tool_result`,
`image`, `document`, and `native`.

Blank search text disables that dimension. Other text is one literal FTS
phrase under the canonical message index, matching the search query pack;
operators and quotes do not become a query language.

The response reports contract version `1`, `atCommitSeq`,
`order: "newest_first"`, exact filtered `total`, and one bounded page. Timed
messages sort by qualified source time, then a deterministic source-object,
generation, source-cursor, run, and message identity key. That key preserves
per-object source order when timestamps tie without claiming causality between
independent objects. Untimed messages follow timed history under the same stable
source key. The default page size is 30, valid limits are 1 through 200, and
canonical content JSON is bounded to 16 MiB per response.

## Canonical rows and accepted differences

The stable timeline row is one canonical message envelope, not one display
fragment. Its ordered common content-block array stays intact. Raw/native JSON
is deliberately omitted from the hot page and remains available through
`getMessages`/detail reads.

This differs from the TypeScript compatibility projection in three named
ways:

- `canonical_timeline_rows_are_message_envelopes`: an assistant message with
  text, thinking, and calls is one stable row rather than several UI rows;
- `canonical_tool_results_are_ordered_blocks`: a result remains its own typed
  block instead of mutating an earlier tool-call row during a read;
- `canonical_timeline_merges_explicit_runs`: root and delegated messages share
  one canonical page rather than requiring a parent page plus independently
  offset-paged branch calls.

These are intentional corrections to persistence/query ownership. A client
may split blocks or visually nest branches as presentation, but those shapes
do not become canonical database identities.

## Branch joins

Each message carries its run, optional parent run, common delegation kind and
strength, resolution status, native child/task identifiers, and optional
label/agent type. When a delegation was resolved from a native spawn, the
query joins `canonical_delegations.decisive_spawn_fact_id` to the exact
`canonical_delegation_spawns.decisive_fact_id` and returns that spawn's
currently materialized `branchAnchorMessageId`.

The query does not correlate by path, adapter ID, callback timing, or a loose
task-ID search. Layout-only and pending relations remain truthful and simply
have no native spawn anchor.

## Facets, projection, snapshots, and cancellation

Facets always describe the complete verified session, before request filters,
so selecting a filter does not erase the available navigation choices. Role,
native-kind, and branch counts count message envelopes. Content-kind counts
count blocks, and tool-name counts count tool-call blocks. `total` separately
counts filtered message envelopes.

Schema version 33 adds `canonical_message_content_blocks`, a narrow
writer-maintained index containing only message/session/run identity, block
ordinal, common content kind, tool name, and native correlation ID. The
message's `content_json` remains authoritative. This avoids aggregate-time
JSON decoding without materializing vendor or presentation semantics. Message
upsert, rewrite, and generation retraction update or cascade this index in the
same writer transaction. Rust and transitional TypeScript schema authorities
carry identical DDL.

Rows, total, and every facet query run in one SQLite read transaction. Opaque
versioned keyset cursors bind every normalized filter, order key, and the
first page's commit watermark. Scope changes, malformed cursors, and a newer
commit are rejected. Cancellation is checked before queue entry and during
SQLite execution; the SDK also rejects an already-aborted signal before N-API
dispatch.

## Conformance evidence

Rust fixtures cover root/delegated merging, exact decisive spawn anchors,
multi-block messages, block/tool/branch facets, include/exclude semantics,
literal search, timed-before-untimed ordering, stable multi-page walks, cursor
scope and watermark rejection, malformed input, payload accounting, query
purity, and pre-queue cancellation. Projection tests cover content-block index
replacement and generation retraction.

The Claude shadow fixture exercises timeline search, delegated metadata,
facets, paging, cursor misuse, payload bounds, and pre-aborted cancellation
end to end through generated N-API declarations and the SDK facade.

## Remaining cutover work

Delegation/workflow queries are recorded in their separate pack. The
consolidated N-API gate measures timeline, facets, total, payload conversion,
ten readers, and concurrent refresh on the scaled history corpus; see the
[Phase 9 query gate](./011-phase-9-query-conformance-benchmark.md).

This remains a shadow query surface until Phase 10. Shared IPC/domain DTOs
and topology benchmarks, operational scale-50/private-corpus soak evidence,
production client migration, and TypeScript SQLite retirement remain. Until
that cutover, the legacy TypeScript timeline remains the production surface
and the Rust observation database remains isolated.
