# RFC 011 Phase 10: durable replay and client subscriptions

Status: complete on 2026-08-12; bounded replay, commit-driven wake-up,
retention resets, cancellation, and delivery metrics are implemented

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

`SpaghettiClient.subscribe()` is an `AsyncIterable` that uses durable replay
as its source of truth and a native commit signal as its idle wake-up. It:

1. replays pages strictly after the requested cursor;
2. yields every non-empty page without overlapping requests;
3. immediately drains another page while `hasMore` is true;
4. advances its private cursor to `(atCommitSeq, u32::MAX)` after a
   complete snapshot, so topic filters do not repeatedly scan irrelevant rows;
5. calls the transport-neutral `waitForCommit()` operation and blocks off the
   JavaScript thread until the sole writer publishes a newer commit;
6. uses a bounded 30-second wait timeout as notification-loss recovery, then
   replays once before renewing the wait. The timeout is configurable from 1
   through 300,000 ms and is not the primary delivery mechanism.

The cursor exposed in each yielded page remains the last delivered change, so
a disconnected caller can persist it and resume with at-least-once delivery.
The private snapshot cursor is only an optimization for the uninterrupted
subscription. Callers deduplicate reconnect delivery by `(commitSeq, ordinal)`,
as required by RFC 011.

`SpaghettiEngineCore` initializes the publisher from the durable watermark and
publishes every successful observation/fact commit through a Tokio watch
channel. N-API awaits that channel without occupying a libuv or query-pool
worker; framed IPC carries the same request/result DTO and cancel frame. No
SQLite connection or query is used while the native wait is pending. Durable
replay after wake-up, and after the bounded recovery timeout, prevents the
process-local signal from becoming the correctness source.

Each subscription owns a cancellation controller. Caller abort and client
disposal stop an in-flight replay or native wait and complete the iterator.
The client rejects malformed pages that claim more data without advancing,
return unordered cursors, disagree with their final cursor, exceed their
snapshot watermark, or move behind the requested durable cursor.

## Verification

Rust tests pin ordered, filtered, paginated, restart-safe replay; snapshot
watermarks; payload accounting; pruning/`ResetRequired`; publisher wake-up and
cancellation; and pre-dispatch cancellation. SDK tests pin:

- multi-page iterator delivery and exact cursor handoff;
- filtered request propagation and bounded batch validation;
- rejection of a non-progressing transport response;
- caller cancellation and disposal while replay is in flight;
- exhaustive N-API dispatch for the expanded method vocabulary;
- real N-API DTO generation and empty-engine replay;
- field-for-field replay parity through direct N-API and framed IPC clients.
- timeout recovery without high-frequency replay polling;
- field-for-field commit-wait and cancellation parity through direct N-API and
  framed IPC clients.

## Metrics and retention

`SpaghettiClient.getSubscriptionMetrics()` returns a fixed-size, process-local
snapshot containing active subscriptions, replay requests and payload bytes,
delivered batches and changes, commit wake-ups, recovery timeouts,
cancellations, and maximum observed commit lag. Rust overview/retention state
continues to expose the oldest retained cursor and retained change-log size.
When a saved cursor predates that window, replay returns structured
`reset_required`; callers read a current snapshot watermark and resubscribe.

The selected Electron topology benchmark remains the reproducible latency and
boundary-byte harness. Scale-50/private-corpus results and release thresholds
are rollout evidence, not unfinished subscription correctness work; they are
tracked in the [Phase 10 closure ledger](./011-phase-10-closure.md).
