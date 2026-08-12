# RFC 011 Phase 9: canonical full-text search query pack

Status: Rust canonical search shadow slice implemented on 2026-08-12

This slice makes canonical message search one coarse Rust operation across
parent and delegated transcripts. It is available through the persistent
engine and isolated Claude observation shadow; the production compatibility
client is not cut over yet. TypeScript neither opens the canonical database
nor queries two indexes, repairs subagent projections, merges scores, sorts,
or slices results.

Timeline/facets and delegation/workflow reads are recorded in their separate
Phase 9 slices. This record alone does not claim the Phase 9 exit gate.

## Query contract

The engine and SDK expose:

```text
search({
  text,
  projectId?, sessionId?,
  adapterIds?, roles?, nativeKinds?,
  branchKind?, cursor?, limit?
})
```

`text` is trimmed and interpreted as one literal FTS5 phrase under SQLite's
default `unicode61` tokenizer. Quotes and operators such as `OR` have no query
language meaning. The contract reports `querySyntax: "literal_phrase_v1"`.
Search text is capped at 4 KiB. Each filter list accepts at most 32 exact,
non-empty values of at most 256 UTF-8 bytes.

`branchKind` is `all` by default and can select `root`, `delegated`, or
`unknown`. `unknown` keeps a message visible while its run/delegation endpoint
is unresolved; it is not silently classified as a root message. A missing
session endpoint similarly leaves project and native session fields absent.
Project/session filters select only resolved matching relations, and supplying
both identities verifies current canonical membership.

The response returns contract version `1`, `atCommitSeq`, an exact total, one
bounded page, and `scoreDirection: "lower_is_better"`. Scores are SQLite FTS5
BM25 ranks from one writer-maintained canonical index, so root and delegated
hits share one score domain. Stable binary message identity breaks equal-score
ties. Scores are meaningful for ordering within the fixed request snapshot,
not as durable values to compare across commits or corpora.

Snippets contain plain source text. The query engine inserts no markup and
separates excerpts with ` … `. A page defaults to 50 rows, rejects limits
outside 1 through 200, and enforces a 4 MiB UTF-8 snippet payload bound.

## Pagination, snapshots, and cancellation

Search uses opaque versioned rank/message-key cursors. A cursor binds the
normalized text, every filter, branch selection, score/key position, and the
first page's commit watermark. Changing scope or observing a newer durable
commit rejects the cursor; callers restart at page one rather than walking a
different ranked snapshot.

Count and page selection execute in one SQLite read transaction on a
read-only/query-only worker. Request cancellation is checked before enqueue,
by the worker epoch, and during SQLite execution through its progress handler.
The SDK also rejects an `AbortSignal` that was already aborted before the
native call, covering the event-delivery edge at the N-API transport boundary.

## Projection and schema contract

`MessageFact` now carries the explicit common run relation. The Claude
adapter emits the root or child run key with each message; common search code
never infers it from adapter IDs or source paths. This semantic change advances
the Claude adapter contract to version 14 so historical transcripts replay.

Schema version 32 adds `canonical_messages.run_key`, its run/activity index,
and the external-content FTS5 table `canonical_message_search_fts`. Insert,
update, and delete triggers maintain that index inside the same writer
transaction as the canonical message. The version bump deliberately rebuilds
older databases because an existing canonical row without its run relation or
FTS entry cannot satisfy the new contract. Rust and transitional TypeScript
schema authorities contain the same DDL.

The canonical query never reads legacy `search_fts` or
`subagent_search_fts`. Those remain compatibility projections until Phase 10.

## Conformance evidence

Rust tests cover literal operator/quote escaping, global and scoped filters,
root/delegated merging, exact totals, stable multi-page walks, cursor scope
and watermark rejection, unresolved endpoints, malformed bounds and
identities, writer-transaction update/delete synchronization, query-only
purity, pre-queue cancellation, and an `EXPLAIN QUERY PLAN` requirement that
uses the FTS virtual-table index.

The small Claude shadow fixture searches the unique nested-workflow marker
end to end through N-API, verifies delegated child metadata, compares its
exact total with the committed TypeScript compatibility oracle, walks a
multi-page shared search, rejects cross-scope cursors, and exercises both
invalid input and a pre-aborted signal.

## Remaining cutover work

Timeline and orchestration are recorded in their separate query packs. The
consolidated N-API gate measures a 325-hit parent-plus-delegated search on
11,394 messages, rapid cancellation, ten readers, and concurrent refresh;
see the [Phase 9 query gate](./011-phase-9-query-conformance-benchmark.md).

This remains a shadow query surface until Phase 10. Shared IPC/domain DTOs
and topology benchmarks, operational scale-50/private-corpus soak evidence,
production client migration, and TypeScript SQLite retirement remain. Until
that cutover, legacy TypeScript search remains the production surface and the
Rust observation database remains isolated.
