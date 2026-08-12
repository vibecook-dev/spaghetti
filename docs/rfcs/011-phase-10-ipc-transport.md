# RFC 011 Phase 10: framed IPC transport and host bridge

Status: transport slice complete on 2026-08-12; consumer and native-host cutover remains

This slice adds the second transport for the asynchronous `SpaghettiClient`.
It preserves the Phase 10 protocol and all accepted Rust query DTOs while
moving calls across a bounded binary channel. It does not select a mandatory
daemon topology, add endpoint discovery, or switch a production consumer.

## Wire boundary

Every message has a fixed ten-byte header followed by a UTF-8 JSON body:

| Bytes | Meaning |
| --- | --- |
| `0..3` | `SPAG` magic |
| `4` | IPC wire version (`1`) |
| `5` | frame kind |
| `6..9` | unsigned big-endian body length |
| `10..` | versioned frame body |

The six frame kinds are connect, connect-result, request, response, cancel,
and close. Decoding rejects bad magic, unsupported wire versions, mismatched
lengths, malformed UTF-8/JSON, unknown frame kinds, unknown query methods,
unknown error codes, and invalid envelope identifiers. The complete frame is
bounded to 24 MiB, which accommodates the native engine's current bounded
ordinary query pages without creating an unbounded IPC allocation class.

Query requests retain the existing 64 KiB limit. The client transport checks
that bound before sending, and the host checks it again before dispatch. N-API
and IPC use the same JSON-byte counter and the same exhaustive method/error
vocabularies, so those boundaries cannot drift independently.

The JSON body is the first portable encoding for MessagePort deployment. The
fixed header and wire-version byte allow a later measured encoding change
without changing query semantics. Durable replay now uses bounded pages whose
raw change payload is capped at 12 MiB, leaving room for base64 expansion and
JSON metadata below the ordinary frame bound. Large exports remain streaming
work; they must not raise that bound.

## Channel and host topology

`SpaghettiIpcChannel` is a minimal binary send/message/close abstraction.
`MessagePortIpcChannel` adapts EventEmitter-style Node/Electron ports and
EventTarget-style browser ports, copies transferred views, and owns listener
cleanup. A future Unix socket, named pipe, or field-native bridge can implement
the same channel without changing `IpcTransport` or `SpaghettiClient`.

`SpaghettiIpcHost` serves one negotiated client channel over an injected
`SpaghettiClientTransport`. In the exercised topology, that backing transport
is `NapiTransport` over one persistent Rust engine:

```text
SpaghettiClient
  -> IpcTransport
  -> one framed MessagePort request
  -> SpaghettiIpcHost
  -> one NapiTransport request
  -> one persistent Rust query operation
```

The host advertises the IPC topology while preserving the engine version,
selected protocol/query-contract versions, supported methods, result DTOs,
and structured errors returned by the backing engine. It never exposes a raw
SQL, database path, migration, or generic native-call endpoint.

## Correlation, cancellation, and lifecycle

The transport keeps bounded maps only for the handshake and currently
in-flight request IDs. Concurrent replies may arrive out of order and are
resolved against their request IDs. Responses for locally cancelled requests
are ignored rather than reviving stale UI state.

An aborted client request settles locally and emits one cancel frame. The host
maps that frame to the backing transport's `AbortSignal`; the existing N-API
transport maps it to the Rust query token. `supersessionKey` therefore has the
same stale-result behavior across both topologies.

The handshake has a configurable timeout (ten seconds by default). Explicit
close, remote port close, malformed input, and host shutdown all settle
pending client work and remove port listeners. Host disposal rejects new
frames, aborts active work, closes the channel, and disposes its backing
transport exactly once when it owns it.

## Verification

`packages/sdk/src/client/__tests__/ipc.test.ts` exercises:

- round-trip coverage for all six frame kinds;
- corrupt magic, length, UTF-8/JSON, and method vocabulary rejection;
- real Node `MessageChannel` negotiation and concurrent out-of-order replies;
- one host dispatch per logical client query;
- cancellation propagation plus late/stale response suppression;
- client-side and host-side request bounds;
- handshake timeout, malformed-channel closure, idempotent disposal, and
  shutdown with a request in flight;
- normalized result parity between direct N-API and IPC clients over the same
  persistent Rust engine;
- structured cursor-error parity across the two transports.

The real-engine parity case compares overview, durable-replay, project-page,
and statistics DTOs field-for-field at the JavaScript object boundary. It then
verifies that an invalid cursor retains the same public `cursor_invalid`
classification.

This is semantic and lifecycle evidence for the portable MessagePort bridge,
not a field-native/daemon performance claim. The selected native IPC endpoint
still needs same-host encoded-byte, latency, event-loop-delay, heap/RSS, and
cancellation-burst benchmark evidence before production cutover.

## Remaining Phase 10 work

- connect this channel contract to the selected field-native/daemon endpoint
  and add IPC topology measurements to the canonical query benchmark;
- migrate CLI/TUI, playground/Electron, React hooks, and SDK examples in
  reversible slices with stale-result tests;
- deprecate compatibility APIs, move the TypeScript oracle to test-only code,
  then remove production `node:sqlite`, query repair, schema, and SQL ownership;
- add the final architecture gate rejecting production bypasses around
  `SpaghettiClient`.
