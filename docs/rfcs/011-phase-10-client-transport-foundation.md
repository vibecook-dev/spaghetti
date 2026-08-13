# RFC 011 Phase 10: asynchronous client and N-API transport foundation

Status: complete on 2026-08-12; the subsequent consumer cutover and retirement
gate are recorded in the [Phase 10 closure ledger](./011-phase-10-closure.md)

This slice introduces the public transport-neutral client boundary required
before production TypeScript consumers stop depending on synchronous SQLite
queries. This document records the foundation slice; the completed final
topology is summarized under **Closure** below.

## Public boundary

`packages/sdk/src/client/` now defines:

- `SpaghettiClient`, the asynchronous semantic query facade;
- one exhaustive request/result map for all 30 canonical Rust read/replay/wait methods;
- versioned request/response envelopes with monotonic request IDs;
- explicit transport and query-contract negotiation;
- a bounded 64 KiB request envelope;
- `SpaghettiClientTransport`, shared by embedded and IPC hosts;
- structured, transport-neutral public errors;
- `NapiTransport`, where one logical client query invokes one asynchronous
  method on the persistent Rust engine;
- `openEmbeddedSpaghettiClient()`, which opens and owns that engine.

`@vibecook/spaghetti-sdk/client` is the portable IPC/client package entry. It
intentionally excludes engine opening and N-API so non-owner processes do not
load the SDK's storage or watcher graph merely to connect to an existing host.

The protocol reuses the accepted Phase 9 canonical Rust DTOs. Domain identity,
ordering, ranking, pagination, totals, watermarks, and capability behavior are
therefore not reimplemented in TypeScript. Versioned query results are checked
against the contract selected at connection time.

The advertised method list is a capability contract. A partial future host may
negotiate successfully, but a call it did not advertise fails locally as
`unsupported_capability` without crossing the transport.

## Cancellation and stale-result suppression

Every query accepts an `AbortSignal`. Pre-aborted requests never dispatch, and
client disposal aborts every in-flight request before closing the transport.
The client also supports `supersessionKey`: starting a newer request with the
same key aborts the older transport request and rejects the older public
promise even if a faulty or remote transport ignores cancellation and later
returns a stale result.

This provides the search-as-you-type primitive needed by CLI, TUI, React, and
Electron consumers without putting display policy in the transport.

## Error boundary

The client exposes `SpaghettiClientError` with stable codes for invalid input,
unsupported capabilities, projection readiness, cursor errors, cancellation,
deadlines, engine shutdown, database ownership/busy state, unavailable
transports, version mismatch, closed transports, and internal failures.

Known native validation failures are classified without exposing SQL, source
paths, or transcript text. Unclassified failures receive only a bounded
transport/request diagnostic ID at the public boundary.

## Ownership

`openEmbeddedSpaghettiClient()` is an owner constructor, not a compatibility
reader. It acquires the Rust database owner lock and optionally starts native
Claude observation. It must use the database and source set assigned to that
owner; it must not be pointed at a database concurrently owned by
`field-native` or a daemon. Owner conflicts return a
structured `database_busy` error rather than silently opening another path.

Callers can inject any negotiated `SpaghettiClientTransport` through
`openSpaghettiClient()`. This is the seam the IPC implementation and consumer
tests use without importing N-API.

## Verification

`packages/sdk/src/client/__tests__/client.test.ts` covers:

- version negotiation and request correlation;
- partial-host capability refusal before dispatch;
- response-envelope and result-contract mismatch;
- structured error propagation and internal-error sanitization;
- pre-dispatch abort, request supersession, and in-flight disposal;
- exhaustive one-call N-API dispatch for all 30 canonical methods;
- a real persistent Rust engine open/query/dispose lifecycle;
- native validation, cursor, and request-payload error mapping.

The Phase 9 query-conformance benchmark remains the semantic oracle for the
underlying result DTOs and performance. This slice added no TypeScript SQLite
read; the later cutover removed the legacy graph from production reachability.

## Closure

CLI/TUI, playground/Electron, SDK, and React database reads are asynchronous
and Rust-backed. React suppresses superseded Promise results; the Electron
renderer uses a structurally checked async client without a compatibility
cast. The TypeScript SQLite implementation is reachable only through the
repository differential-oracle entry, is absent from package artifacts, and
cannot re-enter a production graph under the architecture ratchet.

The completed [utility-process benchmark](./011-phase-10-playground-ipc-benchmark.md)
continues to provide reproducible rollout measurements. Maintainer-selected
scale-50/private runs and release thresholds remain external acceptance
evidence rather than a second production owner.
