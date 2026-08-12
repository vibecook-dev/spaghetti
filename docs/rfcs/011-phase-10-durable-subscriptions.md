# RFC 011 Phase 10: durable replay and client subscriptions

Status: bounded replay and transport-neutral subscription slice complete on
2026-08-12; push publication and retention resets remain

This slice exposes the engine's transactional `change_log` through the same
asynchronous client boundary used by canonical queries. It adds no TypeScript
database reader and does not duplicate projection semantics outside Rust.

## Durable replay contract

`SpaghettiEngine.replayChanges()` and `SpaghettiClient.replayChanges()` return
one snapshot-consistent page ordered by `(commitSeq, ordinal)`. The result
includes:

- the query-contract version and snapshot watermark;
- the oldest currently available durable cursor;
- lossless base64url entity keys and base64 payloads;
- the final returned cursor and whether another page exists;
- raw payload bytes and the enforced payload-byte limit.

Rust validates cursors, topic count, non-empty topic names, and a page limit of
1 through 1,000. A page stops at either that item limit or 12 MiB of raw change
data. The 12 MiB bound leaves room for base64 expansion and JSON metadata under
the IPC transport's 24 MiB frame bound. A single change larger than the page
budget is treated as an engine invariant failure instead of allocating an
unbounded response.

Replay executes on the read-only query pool. Its watermark and rows come from
one SQLite snapshot, and `AbortSignal` reaches the Rust cancellation token in
both embedded N-API and framed IPC topologies.

## Subscription behavior

`SpaghettiClient.subscribe()` is an `AsyncIterable` built entirely from the
durable replay operation. It:

1. replays pages strictly after the requested cursor;
2. yields every non-empty page without overlapping requests;
3. immediately drains another page while `hasMore` is true;
4. advances its private polling cursor to `(atCommitSeq, u32::MAX)` after a
   complete snapshot, so topic filters do not repeatedly scan irrelevant rows;
5. waits 250 ms by default before checking for a newer commit.

The cursor exposed in each yielded page remains the last delivered change, so
a disconnected caller can persist it and resume with at-least-once delivery.
The private snapshot cursor is only an optimization for the uninterrupted
subscription. Callers deduplicate reconnect delivery by `(commitSeq, ordinal)`,
as required by RFC 011.

Polling is the first transport-neutral implementation because no Rust live
publisher exists yet. Replacing the wait with a native/IPC wake-up signal will
not change the public iterator, replay semantics, or durable cursor contract.

Each subscription owns a cancellation controller. Caller abort and client
disposal stop an in-flight replay or polling wait and complete the iterator.
The client rejects malformed pages that claim more data without advancing,
return unordered cursors, disagree with their final cursor, exceed their
snapshot watermark, or move behind the requested durable cursor.

## Verification

Rust tests pin ordered, filtered, paginated, restart-safe replay; snapshot
watermarks; payload accounting; and pre-dispatch cancellation. SDK tests pin:

- multi-page iterator delivery and exact cursor handoff;
- filtered request propagation and bounded batch validation;
- rejection of a non-progressing transport response;
- caller cancellation and disposal while replay is in flight;
- exhaustive N-API dispatch for the expanded method vocabulary;
- real N-API DTO generation and empty-engine replay;
- field-for-field replay parity through direct N-API and framed IPC clients.

## Remaining work

- add a Rust publisher/wake-up path so idle subscriptions do not poll while
  preserving durable replay as the source of truth;
- define change-log retention and return RFC 011 `ResetRequired` when a saved
  cursor predates `oldestAvailable`;
- add subscriber lag, replay bytes, cancellation, and polling/wake-up metrics;
- migrate consumers to the asynchronous client and its durable invalidation
  stream in reversible slices;
- benchmark replay and live-delivery latency through the selected production
  IPC endpoint before cutover.
