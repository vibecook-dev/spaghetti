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

### P10-06 — broad-rollout evidence remains a release-policy input

The reproducible Phase 9 N-API and Phase 10 Electron IPC benchmarks accepted
small and scale-10 observations. The later production-host pass added bounded
native storage/source/query telemetry and completed a frozen 3.86 GB private
corpus with truthful readiness: 1,112,311 source records, 5,138,709 facts,
69,041 commits, zero retries, and ready equal to converged. The first truthful
run took 634.37 seconds; the optimized revision takes 574.80 seconds with the
same complete counts. The private reports stay outside the repository.
Scale-50 policy and reviewed release thresholds remain maintainer-owned rollout
inputs; they are not a reason to preserve a second database authority and
cannot be fabricated by code changes.

**Closure disposition:** the reproducible harness and report schema remain in
the repository, and a large private production reference now exists.
Publication of private data and release-policy thresholds remain explicitly
maintainer-owned. They do not preserve a second database authority or leave a
Phase 10 code path open.

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

### P10-09 — playground benchmark retained migration-era selectors

A follow-up playground audit found that the Electron topology benchmark still
passed an obsolete `engine: 'ts'` field and routed its real Rust database
through an `observationShadow` option. The engine field had no runtime effect,
but `src/benchmark` was outside the playground TypeScript project, so the stale
option was not rejected. Current utility configuration and diagnostics also
retained unused shadow-era names after the unconditional production cutover.

**Resolution:** the benchmark now supplies one canonical `dbPath`; the shadow
option and environment-variable seam are deleted; host diagnostics use
production-owner terminology; and `src/benchmark` is included in the
playground TypeScript project. A one-run real Electron topology smoke test
negotiated `playground-utility` and returned the expected fixture counts of
three projects, 20 sessions, and 300 messages through the Rust owner.

### P10-10 — cold ingest looked idle while native message storage amplified input

A real playground cold start exposed two coupled production defects. After
roughly 468 MiB of transcript input, the incomplete SQLite cache had already
grown to roughly 3 GiB even though the Rust process was still actively
scanning. Message-native JSON was stored once in `canonical_messages` and again
inside the fact audit; Serde encoded both the duplicated `Vec<u8>` payload and
repeated entity keys as JSON integer arrays. At the same time, startup progress
was a one-shot renderer event with no late-subscriber snapshot, no per-adapter
heartbeat, and shutdown waited for initialization before cancelling it. The
playground therefore appeared stuck at ingest while the cache continued to
consume disk.

The first compact-storage pass removed the integer-array encoding and the
second native-message copy, but a bounded real replay exposed one more layer:
at 402 MB (decimal) of committed append cursors, the 1.9 GiB cache still
contained 339 MiB of normalized fact-audit JSON and a 116 MiB
`entity_key, fact_kind` index
that no query consumed. This was contrary to the stream contract: Claude,
Codex, and Grok transcript streams declare `HashOnly`, while the projector was
persisting every normalized fact body as though they declared `Full`.

**Resolution:** schema v40 carries the declared raw-retention policy through
the commit context and records it on `source_streams`. `fact_records` is now a
provenance/ownership ledger: `None` and `HashOnly` store an explicit empty
`omitted` payload alongside the durable fact identity, fact kind, source
object/generation/cursor, record hash, ordinal, observation time, and commit
sequence. Semantic entity identity remains in canonical/assertion projections;
the nullable generic-ledger copy is omitted by the later amplification pass
described below. `DiagnosticExcerpt` does the same for ordinary facts and
retains only an already-redacted bounded unknown-record shape; `Full` opts into
a compressed fact body. The unused wide fact index is removed; the
source-instance and object/generation indexes required by inventory and
retraction remain. Lossless canonical message detail stays available as the
sole native-message copy, with bounded zstd compression and compact entity-key
serialization. A stale schema is rebuilt and vacuumed before cold ingest. The
sole writer rejects commits before the filesystem crosses a bounded 1–4 GiB
reserve instead of filling the volume.

Observation startup publishes structured per-adapter state and periodic
heartbeats, caches the latest snapshot for late subscribers and renderer status
polling, settles explicitly on ready, and aborts native initialization before
awaiting shutdown. Regression coverage locks the retention-aware single-copy
storage shape, compression and decode bounds, durable retention value, removal
of the unused index, legacy decoding, schema-space reclamation, disk-reserve
calculation, startup lifecycle, and renderer source-state mapping.

A real schema-v40 Electron replay reached 406,865,997 committed append-cursor
bytes with 844,003 facts and 197,233 canonical messages. The database plus its
live WAL occupied 1,535,231,176 bytes (1.43 GiB), roughly one quarter below the
v39 footprint at the comparable cursor. Every fact row used the explicit
`omitted` codec and the aggregate fact-audit payload was zero bytes. The replay
was then stopped through the normal Electron lifecycle; SQLite `quick_check`
returned `ok`, and the resumable cache was retained.

The later physical-amplification pass narrowed the generic ledger one step
further. Canonical and assertion projections already retain semantic entity
keys and reference `fact_records` by `fact_id`; no production query consumed
the second `fact_records.entity_key` copy. New rows therefore leave that
nullable compatibility column empty while retaining fact kind, complete source
provenance, record identity, retention codec, and commit ownership. An
entity-only 64k spike reduced the database from 375.5 MiB to 342.9 MiB without
a measurable runtime change. Existing databases remain compatible and reclaim
the space on rebuild; no in-place semantic migration is required.

### P10-11 — the retained benchmark did not measure production cold ingestion

The performance follow-up found that the existing `bench:ingest` command still
measured the retired native bulk/oracle route and a small fixture. It did not
open the production observation host, exercise adapter and projection work, or
wait for all bounded startup passes to converge. On the production route, a
deterministic 16,384-record transcript initially took 30.24 seconds, and an 8x
corpus showed strongly superlinear growth.

**Resolution:** `bench:observation` now drives the real asynchronous owner,
asserts convergence and canonical row counts, retains one host for live
percentile samples, and reports durable database/change-log metrics. The first
optimization wave removes a 64-record coordinator bottleneck, gates
generation-replacement work, adds matching cleanup indexes, reuses statements,
coalesces repeated aggregates, skips provably unrelated projectors, applies
bounded cache/mmap/checkpoint settings, and makes startup readiness wait for
known backlog. The best implementation point reduced 16,384 records to 2.08
seconds; the final checkpoint-balanced configuration takes 2.58 seconds (11.7x
faster than the original) while measuring single-record live append at p50 6.0
ms / p99 8.2 ms and 64-record bursts at p50 19.1 ms / p99 91.4 ms. The remaining
16k-to-131k scaling curve still failed the new acceptance gate at that point.

The next measured pass aligned reducer indexes with their actual ordering,
removed superseded query indexes, made checkpoints explicitly writer-owned,
and added a durable size-gated query bootstrap. A frozen-corpus audit then
found and fixed object retries being acknowledged at instance scope and a
polling race across deferred FTS/index finalization. That lifecycle build
measured 1.591 seconds at 16,384 records and 8.299 seconds at 65,536 records, a
passing 5.217x ratio.

The subsequent storage/set-write pass rejected a neutral source-record
normalization (89.91 MiB versus 89.88 MiB for the complete 64k structures),
omitted the unused ledger entity-key copy, and batched fact, canonical-message,
and cold content-block writes within fixed bounds. Five-run medians are 1.176
seconds at 16,384 records and 6.140 seconds at 65,536 records, a passing 5.220x
ratio, with 86.4/343.0 MiB databases. A 100-sample one-record live repeat
measures p50 1.5 ms and p99 3.0 ms. The 32-large-object gate completes all
131,104 messages at ready/convergence in 11.84 seconds, and a 64k warm reopen
performs zero writes with 7.7 ms median readiness. The 3.86 GB private gate
retains the exact complete counts and zero retries at 574.80 seconds with a
9,214.1 MiB durable database, 9.4% faster and 6.1% smaller than the first
truthful reference.

The [performance optimization design](./011-performance-optimization-plan.md)
records the evidence, rejected spikes, non-negotiable correctness constraints,
and staged plan. Metrics, index alignment, writer-owned checkpoints, durable
bootstrap, provenance compaction, and bounded set-oriented history writes are
implemented. A broader integer-surrogate schema and more invasive reducer
arrangements would be new evidence-gated work, not incomplete Phase 10 scope.
Phase 10's Rust ownership exit remains complete; release-policy acceptance is
kept separate from the code-ownership exit.

## Verification

The complete closure tree passed these local gates through 2026-08-13:

- `pnpm typecheck`, `pnpm lint`, `pnpm format:check`, `pnpm validate`, and
  `pnpm build`;
- SDK package build and artifact scan: 16 emitted files and exactly four
  rolled public declarations;
- SDK tests: 319 total, 312 passed, seven environment-appropriate skips, zero
  failures;
- CLI tests: 110 passed, zero failures;
- `cargo fmt --all -- --check`, workspace/all-feature `cargo check`, Clippy
  with warnings denied, and 531 Rust tests;
- Phase 9 query conformance: all 12 groups passed, including the 12 MiB
  payload-boundary probe;
- Claude, Codex, and Grok observation parity is exact across cold, live,
  reconcile, generation-replacement, and restart scenarios;
- the 32-large-object, warm-restart, 100-sample live, and frozen 3.86 GB
  production-host gates passed with truthful readiness; the optimized private
  gate completed at 574.80 seconds with zero retries and the exact complete
  durable counts;
- the post-build Electron utility-process smoke negotiated the Rust owner,
  returned 3 projects, 20 sessions, and 300 messages, and recovered after 100
  cancelled requests;
- architecture ownership ratchet: every rule has zero active and zero
  allowlisted violations.

## Acceptance matrix

| Requirement                                     | Evidence                                                  | Status                                              |
| ----------------------------------------------- | --------------------------------------------------------- | --------------------------------------------------- |
| Async public compatibility and React APIs       | TypeScript API/hook tests and package declarations        | Complete                                            |
| CLI/TUI queries cross Rust-backed service       | production imports plus command/view tests                | Already satisfied                                   |
| Electron utility owns one Rust engine           | utility lifecycle and framed transport tests              | Already satisfied                                   |
| N-API and IPC normalized parity                 | real-engine transport suite                               | Already satisfied                                   |
| Cancellation and stale suppression              | client, IPC, React tests                                  | Complete                                            |
| Durable retention and reset                     | Rust replay-retention tests                               | Already satisfied                                   |
| Wake-driven subscriptions and metrics           | Rust publisher plus client/IPC tests                      | Complete                                            |
| No production TypeScript SQLite/query authority | architecture ratchet and built artifact scan              | Complete                                            |
| Query conformance and query-only purity         | Phase 9 conformance harness                               | Already satisfied                                   |
| Reference rollout evidence                      | reproducible commands plus private production-host report | Partially accepted; release policy remains external |

All code/ownership rows satisfy the Phase 10 exit contract. The accepted
private-corpus observation establishes a production reference; scale-50 policy
and reviewed thresholds remain rollout evidence for a release decision. They
are not a second implementation or a Phase 10 ownership blocker.
