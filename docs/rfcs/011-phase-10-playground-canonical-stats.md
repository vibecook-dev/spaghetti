# RFC 011 Phase 10: first playground renderer cutover

Status: canonical catalog statistics exposed to the renderer on 2026-08-12;
broader product reads and production-owner promotion remain

This slice moves the first user-visible playground read onto the canonical
`SpaghettiClient` path. When the opt-in RFC 011 observation owner is available,
the Settings dialog shows its catalog commit, searchable-message count, active
source-object count, and allocated database bytes. The values retain the
versioned Rust `getStats` DTO rather than being relabeled as legacy segments.

## Boundary

```text
Settings dialog
  -> context-isolated preload method
  -> Electron main readCanonicalStats()
  -> one shared SpaghettiClient
  -> framed MessagePort
  -> SDK UtilityProcess
  -> non-owning NapiTransport
  -> persistent Rust observation owner
```

Electron main opens the framed connection lazily and shares the negotiated
client across product reads. Concurrent first callers share the same opener.
Utility-process exit, explicit kill, and graceful shutdown invalidate the
cached connection; a later read after automatic host restart negotiates a new
one. Independent benchmark and lifecycle callers may still request their own
connection explicitly.

The renderer receives the canonical stats DTO directly through its existing
context-isolated structured-clone bridge. The preload remains a one-line
forwarder, and neither preload nor renderer imports a runtime SDK owner module.
The main-process query function accepts only a canonical client provider and
calls `SpaghettiClient.getStats()` exactly once.

The typed legacy utility RPC explicitly excludes main-owned methods, currently
canonical stats and live Git worktree discovery. Its dispatcher is exhaustive,
so this read cannot accidentally be forwarded to `SpaghettiAPI.getStats()` or
added as another compatibility-service query.

## Compatibility decision

Canonical statistics are shown in a separate **Canonical observation** section
rather than replacing the header's compatibility-cache counts. During this
phase the canonical owner observes Claude data while the current playground
may aggregate Claude, Codex, and Grok. `searchableMessages` is also not the same
concept as legacy `totalSegments`. Combining or renaming those values would
create false parity.

When observation is disabled, the section says so and performs no canonical
query. Its availability check reads only in-memory utility lifecycle state; it
does not build the full parity report or touch either database. Starting and
running owners may be queried because port attachment already waits for owner
startup. Failed or stopped owners retain their structured status without
opening another client. Detailed health can still classify a running owner as
degraded without changing this lifecycle check.

## Verification

- a focused boundary test proves one product request opens the provider once
  and invokes canonical `getStats` once;
- the production-built topology benchmark now uses the same shared-client
  opener, verifies a second acquisition returns the same client, and reads
  stats through the product query function;
- TypeScript covers the main, preload, shared bridge, and renderer DTO end to
  end;
- the RFC 011 architecture ratchet now rejects direct native-engine queries
  from the migrated product module and continues to reject owner-SDK runtime
  imports from Electron main and preload.

## Remaining work

- migrate project/session/search or timeline surfaces only after their
  canonical identity and display DTOs preserve the current multi-source
  navigation contract;
- map durable change topics to bounded renderer invalidations and remove the
  migrated surface's legacy refresh dependency;
- promote the Rust observation owner out of opt-in shadow mode after rollout
  behavior and recovery are defined;
- shrink the legacy query and SQLite ownership allowlists as each read moves.
