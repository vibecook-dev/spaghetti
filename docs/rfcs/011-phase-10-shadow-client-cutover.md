# RFC 011 Phase 10: canonical shadow client cutover

Status: first canonical consumer cutover complete on 2026-08-12; user-facing
legacy consumers remain

This slice migrates the existing Claude observation shadow from direct N-API
query calls to the transport-neutral `SpaghettiClient`. The shadow powers the
playground's opt-in canonical diagnostics and the query-conformance benchmark,
so this is a real consumer boundary without changing legacy CLI or UI result
contracts prematurely.

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

All 28 canonical query methods used by the shadow facade now cross request-ID
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

## Remaining work

- migrate a user-facing CLI or playground read whose canonical DTO already
  preserves its product contract;
- replace legacy playground change forwarding with durable client
  subscriptions once topic payloads have a public invalidation mapping;
- connect the framed transport to the selected field-native/daemon endpoint;
- remove each migrated module from the legacy ownership allowlists and retire
  TypeScript SQLite only after no production read bypass remains.
