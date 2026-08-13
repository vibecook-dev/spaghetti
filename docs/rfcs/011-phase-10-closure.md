# RFC 011 Phase 10 closure ledger

Status: Phase 10 code and ownership exit complete on 2026-08-12; maintainer
rollout evidence remains explicitly tracked below

This is the durable closure record for RFC 011 Phase 10. It audits the shipped
SDK, CLI/TUI, Electron playground, N-API transport, framed IPC transport, and
repository-only differential tooling against the Phase 10 plan and exit gate.
It distinguishes production ownership blockers from rollout evidence that
requires a maintainer-selected machine or private corpus.

## Exit contract

Phase 10 exits only when production TypeScript cannot open or query the
Spaghetti database and every canonical database operation crosses the
asynchronous `SpaghettiClient` boundary to Rust. The public database-backed API
must be asynchronous, N-API and IPC must preserve the same semantics and
cancellation behavior, and CI must reject a return to TypeScript SQLite
ownership.

## Findings

### P10-01 — public compatibility API and React entry were synchronous

At audit time, the package root exported the synchronous `SpaghettiAPI` type.
The published React provider accepted that type, the live hooks called
database-backed methods during synchronous React snapshots, and
`AgentDataPlayground` treated query results as immediate values. This
contradicted the Phase 10 API migration even though Rust already owned the
production database behind the compatibility facade.

**Resolution:** every public database-backed compatibility method now returns a
`Promise`; the old synchronous contract moved behind the repository oracle;
and React hooks/components use asynchronous effects with serialized refresh
and stale-result suppression. Focused tests cover supersession, disposal, and
durable invalidation filtering.

### P10-02 — Electron renderer hid Promise results behind an unsafe cast

At audit time, `apps/playground/src/renderer/src/ipc-api.ts` cast
Promise-returning IPC methods to the synchronous `SpaghettiAPI` contract. The
file documented that callers must ignore the declared return type. This was an
unsound public boundary that made an accidental synchronous React read compile.

**Resolution:** `SpaghettiReactClient` is a structurally checked async surface,
the provider accepts `client`, the double cast is deleted, and renderer
readiness supports the asynchronous IPC bridge.

### P10-03 — durable subscriptions polled and exposed no subscriber metrics

At audit time, `SpaghettiClient.subscribe()` replayed the change log every 250
ms while idle. Rust had no commit publisher/wait primitive, and the client did
not expose subscriber lag, replay bytes, wakeups, timeouts, or cancellation
counts. Retention pruning and `ResetRequired` were already implemented and
tested.

**Resolution:** the sole Rust writer publishes committed sequence advancement
through a Tokio watch channel. The cancellable `waitForCommit` request is
shared by N-API and IPC; subscriptions block without consuming JavaScript,
libuv, or query-pool workers and retain a bounded timeout replay only for
lost-notification recovery. Fixed-size local
metrics expose subscriber lag, replay bytes, delivery, wake-up, timeout, and
cancellation counters. Retention pruning and `reset_required` remain the
durable reconnect contract.

### P10-04 — published declarations contained the repository-only oracle

At audit time, runtime package exports no longer reached the TypeScript
SQLite/query engine, but the SDK declaration build emitted declarations for
every file under `src`, including `legacy-oracle`, `SqliteService`, schema,
migrations, source watchers, and projection writers. Export maps prevented
supported runtime imports, but the shipped artifact still advertised
implementation that Phase 10 calls test-only.

**Resolution:** the SDK rolls declarations into exactly the four public entry
files. `scripts/check-sdk-package.mjs` validates every built runtime/declaration
artifact and every export-map target, rejecting legacy-oracle, SQLite owner,
schema, watcher, or relative declaration leaks. The architecture ratchet also
requires that build configuration and rejects synchronous React/renderer
bypasses. The source oracle remains explicit repository differential tooling;
it is not a production dependency or supported package surface.

### P10-05 — phase records described superseded shadow ownership

At audit time, the individual Phase 9 and Phase 10 slice records still said
that the legacy TypeScript service was the production read owner, the Rust
owner was opt-in, and CLI/TUI/playground migration remained. Commit `30a3c92`
had superseded those claims.

**Resolution:** every Phase 9 record marks its shadow language as historical
and links here; every Phase 10 slice records the final Rust-owned topology while
preserving its historical benchmark observations.

### P10-06 — broad-rollout evidence is not yet accepted

The reproducible Phase 9 N-API and Phase 10 Electron IPC benchmarks have
accepted small and scale-10 observations. Scale-50, maintainer-approved private
corpus results, deeper native timing/allocation telemetry, and reviewed release
thresholds remain rollout evidence. They are not a reason to preserve a second
database authority and cannot be fabricated by code changes.

**Closure disposition:** the reproducible harness and report schema remain in
the repository. Private-data execution and release-policy thresholds are
explicitly maintainer-owned. They do not preserve a second database authority
or leave a Phase 10 code path open.

### P10-07 — repository benchmarks still confused the public and oracle APIs

The final root typecheck found that `scripts/bench-queries.ts` still annotated
its synchronous differential service as the now-asynchronous public
`SpaghettiAPI`. A wider source sweep then found an unreferenced pre-RFC
`scripts/benchmark.ts` that directly measured the retired TypeScript owner and,
by default, deleted and rebuilt the user's legacy cache. It was neither an
isolated parity comparison nor part of a validation gate.

**Resolution:** the conformance harness now names `LegacySpaghettiAPI`
explicitly through the repository-only oracle entry. The obsolete destructive
benchmark is deleted; the supported Rust-vs-oracle query harness and Electron
topology harness remain. `ObservationService` explicitly extends the public
async `SpaghettiAPI`, so their contracts cannot silently diverge.

### P10-08 — pre-RFC reference documents still claimed current ownership

Several parser, live-update, and TUI design records still labeled the former
dual TypeScript/Rust SQLite topology as current, or presented synchronous
`SpaghettiAPI` snippets without a historical boundary. Although those files
were not runtime-reachable, they could send future implementation work back
toward the retired architecture.

**Resolution:** the records remain available for design provenance, but now
carry explicit RFC 011 supersession notices and link to this closure ledger.
Current Phase 9/10 records describe the Rust-owned asynchronous topology.

## Verification

The complete closure tree passed these local gates on 2026-08-12:

- `pnpm typecheck`, `pnpm lint`, `pnpm format:check`, `pnpm validate`, and
  `pnpm build`;
- SDK package build and artifact scan: 16 emitted files and exactly four
  rolled public declarations;
- SDK tests: 318 total, 311 passed, seven environment-appropriate skips, zero
  failures;
- CLI tests: 110 passed, zero failures;
- `cargo fmt --all -- --check`, workspace/all-feature `cargo check`, Clippy
  with warnings denied, and 504 Rust tests;
- Phase 9 query conformance: all 12 groups passed, including the 12 MiB
  payload-boundary probe;
- architecture ownership ratchet: every rule has zero active and zero
  allowlisted violations.

## Acceptance matrix

| Requirement | Evidence | Status |
| --- | --- | --- |
| Async public compatibility and React APIs | TypeScript API/hook tests and package declarations | Complete |
| CLI/TUI queries cross Rust-backed service | production imports plus command/view tests | Already satisfied |
| Electron utility owns one Rust engine | utility lifecycle and framed transport tests | Already satisfied |
| N-API and IPC normalized parity | real-engine transport suite | Already satisfied |
| Cancellation and stale suppression | client, IPC, React tests | Complete |
| Durable retention and reset | Rust replay-retention tests | Already satisfied |
| Wake-driven subscriptions and metrics | Rust publisher plus client/IPC tests | Complete |
| No production TypeScript SQLite/query authority | architecture ratchet and built artifact scan | Complete |
| Query conformance and query-only purity | Phase 9 conformance harness | Already satisfied |
| Reference rollout evidence | committed benchmark reports/commands | External evidence remains |

All code/ownership rows satisfy the Phase 10 exit contract. Scale-50, accepted
private-corpus observations, and reviewed thresholds remain rollout evidence
for a release decision and for RFC-wide performance acceptance; they are not a
second implementation or a Phase 10 ownership blocker.
