# RFC 011 Phase 10: playground utility-process IPC endpoint

Status: complete on 2026-08-12; this endpoint is the playground's production
owner boundary and final evidence is indexed by the
[Phase 10 closure ledger](./011-phase-10-closure.md)

This slice connects the Phase 10 framed transport to the existing Electron
utility-process ownership boundary. It gives Electron main a real negotiated
`SpaghettiClient` without adding canonical query methods to the legacy RPC
protocol or exposing N-API in the main/renderer processes.

Electron main imports `@vibecook/spaghetti-sdk/client`, a dedicated portable
package entry containing only the semantic protocol, client, framing, channel,
and IPC transport. The embedded opener and `NapiTransport` remain on the SDK
owner-side entry. Architecture ratchets walk the portable entry's runtime
import graph and reject storage, watcher, native-addon, or non-client source
dependencies; they also prevent Electron main/preload from runtime-importing
the owner SDK bundle.

## Topology

```text
Electron main
  `- SdkHostClient.getObservationClient()
       |- MessageChannelMain.port2
       |- MessagePortIpcChannel -> IpcTransport -> SpaghettiClient
       `- transfer port1 with attach-spaghetti-client
            -> SDK UtilityProcess
                 -> SdkRuntime.attachObservationClient()
                 -> ClaudeObservationShadow.serveIpc()
                 -> SpaghettiIpcHost
                 -> non-owning NapiTransport
                 -> one Rust SpaghettiEngine owner
```

The existing structured-clone RPC remains the control and product bridge
plane. It transfers a port and acknowledges attachment, but it does not carry
canonical query DTOs. After negotiation, every canonical operation uses the
versioned binary frame, request ID, error vocabulary, payload bounds,
cancellation path, and one-call semantic API shared by all Phase 10 clients.

## Cold start and ownership

The utility process announces control-plane readiness before cold ingest so
Electron main does not mistake a busy native scan for a failed fork. A query
attachment may therefore arrive before the observation owner is ready.
`attachObservationClient()` starts the runtime idempotently, waits for initial
ingest and shadow startup, and leaves the raw `MessagePort` paused until the
framed host listener is installed. The control-plane acknowledgement is sent
only after that installation. This preserves an early connect frame instead
of dropping it and avoids starting the client's 10-second negotiation timeout
during cold ingest.

Each connection gets a new `SpaghettiIpcHost` and a non-owning
`NapiTransport`. The shadow tracks all hosts, closes them before its internal
client and Rust engine, and rejects attachments once disposal begins. Closing
one client never shuts down the shared engine. Utility-process exit closes the
port, so pending client work settles as `transport_closed`; reconnection is an
explicit new negotiation after the utility host restarts.

## Evidence

- the real shadow lifecycle test negotiates a framed client against the same
  persistent Rust owner and compares overview, project, and durable replay
  watermarks;
- the same test proves owner disposal closes the attached client;
- the utility-runtime test starts attachment before runtime initialization,
  negotiates through a queued early frame, and queries the observed fixture;
- disabled mode rejects and closes an attempted attachment;
- SDK and playground typechecks plus the Electron production build cover the
  Node/Electron `MessagePort` boundary; the built portable entry is free of
  SQLite, watcher, and native-addon imports;
- the [selected-topology benchmark](./011-phase-10-playground-ipc-benchmark.md)
  exercises the production-built `MessageChannelMain`/UtilityProcess boundary,
  exact encoded frame bytes, latency, event-loop delay, utility RSS/heap,
  cancellation, recovery, and a 12.6 MiB bounded response.

## Closure

The renderer's project/session/message/search/statistics reads now use an
explicit asynchronous client, durable subscriptions invalidate product
snapshots, and `SdkRuntime` creates the Rust observation service
unconditionally. The repository-only TypeScript oracle is outside the utility,
main, preload, renderer, and published package graphs. Reviewed scale/private
thresholds remain rollout policy, using the committed topology report format.
