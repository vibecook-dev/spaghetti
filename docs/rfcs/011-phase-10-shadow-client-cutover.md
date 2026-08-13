# RFC 011 Phase 10: canonical shadow client cutover

Status: complete on 2026-08-12; this first-consumer slice was followed by the
full production cutover in the [Phase 10 closure ledger](./011-phase-10-closure.md)

This slice migrates the existing Claude observation shadow from direct N-API
query calls to the transport-neutral `SpaghettiClient`. The shadow powers the
playground's opt-in canonical diagnostics and the query-conformance benchmark,
so this was the first real consumer boundary without prematurely changing CLI
or UI result contracts. The historical `ObservationShadow` name is retained
only for differential APIs; it is not the current production ownership mode.

## Ownership split

`openClaudeObservationShadow()` still opens one persistent `SpaghettiEngine`
because that handle owns observation lifecycle, source watchers, refresh, and
shutdown. After the initial observation scan, it creates a non-owning
`NapiTransport` and negotiates one `SpaghettiClient` over the same engine:

```text
ClaudeObservationShadow
  |- lifecycle/status -> SpaghettiEngine owner handle
  `- every read       -> SpaghettiClient -> NapiTransport -> engine query pool
```

The client is disposed before the engine. Startup failure disposes whichever
resources were acquired, concurrent shadow disposal remains idempotent, and
the transport never independently owns or shuts down the engine.

All canonical query methods used by the shadow facade cross request-ID
correlation, protocol/query-contract negotiation, request bounds,
`AbortSignal`, result-contract checks, and structured error normalization. The
facade keeps its existing DTOs and method names so the playground and benchmark
need no display or semantic adapter.

## Stable error boundary

Native `EngineError` categories now choose meaningful N-API statuses:

- invalid configuration/query/commit -> `InvalidArg`;
- request cancellation -> `Cancelled`;
- saturated query queue -> `QueueFull`;
- engine shutdown -> `Closing`.

`NapiTransport` converts those statuses to the same public error vocabulary as
IPC. Cursor failures remain the more specific `cursor_invalid`; other native
validation failures become `invalid_request`; queue saturation becomes a
retryable `database_busy` classification; cancellation and shutdown become
`cancelled` and `engine_stopping`. Raw SQL, source paths, and native validation
details do not cross the public client boundary.

## Regression gate and evidence

The RFC 011 architecture ratchet now rejects direct native query calls in
`observation-shadow.ts`. Engine lifecycle methods are intentionally allowed.
Adding the same rule to each subsequently migrated consumer makes cutover
monotonic rather than conventional.

The shadow's real-engine suite exercises project/session history, messages,
search, timelines, orchestration, capability packs, runtime, teams/inboxes,
sources, statistics, and usage through the client. It also asserts negotiated
N-API capabilities, stable invalid-request/cursor/cancellation errors, and
idempotent owner disposal. The canonical conformance benchmark now measures
and compares the client facade rather than a direct engine shortcut.

## Closure

The user-facing playground, CLI/TUI, SDK, and React surfaces subsequently moved
to the production Rust observation service. Durable replay now drives bounded
invalidation, and the utility process unconditionally owns the playground
database. Empty legacy ownership allowlists, source-graph checks, and the built
package scan prevent a production read bypass from returning. The
[utility-process benchmark](./011-phase-10-playground-ipc-benchmark.md) remains
the rollout evidence harness.
