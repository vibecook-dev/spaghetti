# RFC 011 Phase 10: asynchronous client and N-API transport foundation

Status: foundation and first consumer/utility endpoint complete on 2026-08-12;
broad consumer cutover remains

This slice introduces the public transport-neutral client boundary required
before production TypeScript consumers can stop depending on synchronous
SQLite queries. It does not switch a production consumer or retire legacy
database ownership yet.

## Public boundary

`packages/sdk/src/client/` now defines:

- `SpaghettiClient`, the asynchronous semantic query facade;
- one exhaustive request/result map for all 29 canonical Rust read/replay methods;
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
owner; it must not be pointed at a database concurrently owned by the legacy
TypeScript service, `field-native`, or a daemon. Owner conflicts return a
structured `database_busy` error rather than silently opening another path.

During the remaining migration, callers can also inject any negotiated
`SpaghettiClientTransport` through `openSpaghettiClient()`. This is the seam
the IPC implementation and consumer tests use without importing N-API.

## Verification

`packages/sdk/src/client/__tests__/client.test.ts` covers:

- version negotiation and request correlation;
- partial-host capability refusal before dispatch;
- response-envelope and result-contract mismatch;
- structured error propagation and internal-error sanitization;
- pre-dispatch abort, request supersession, and in-flight disposal;
- exhaustive one-call N-API dispatch for all 29 canonical methods;
- a real persistent Rust engine open/query/dispose lifecycle;
- native validation, cursor, and request-payload error mapping.

The Phase 9 query-conformance benchmark remains the semantic oracle for the
underlying result DTOs and performance. This slice adds no TypeScript SQLite
read and does not change the legacy production surface.

## Remaining Phase 10 work

- use the completed
  [playground utility-process benchmark](./011-phase-10-playground-ipc-benchmark.md)
  to establish rollout thresholds, then promote the owner out of opt-in shadow
  mode;
- continue the first
  [playground/Electron renderer read](./011-phase-10-playground-canonical-stats.md),
  then migrate CLI/TUI, React hooks, and SDK examples in reversible slices with
  stale-result tests;
- deprecate compatibility APIs, move the TypeScript oracle to test-only code,
  then remove production `node:sqlite`, query repair, schema, and SQL ownership;
- add the final architecture gate rejecting production bypasses around
  `SpaghettiClient`.
