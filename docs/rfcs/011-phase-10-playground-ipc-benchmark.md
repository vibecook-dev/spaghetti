# RFC 011 Phase 10: playground IPC topology benchmark

Status: selected-topology benchmark complete on 2026-08-12; code cutover is
complete, while reviewed regression thresholds and broader rollout evidence
remain maintainer acceptance inputs

This record measures the actual Electron topology selected for the playground:
an Electron-main `SpaghettiClient` sends versioned binary frames over
`MessageChannelMain` to the SDK UtilityProcess, which owns one persistent Rust
observation engine. It is not an in-process `MessageChannel` proxy or an N-API
microbenchmark.

## Reproducible harness

The production build contains a dedicated benchmark entrypoint:

```bash
# Build the SDK and Electron entries, then run the default topology matrix.
pnpm bench:query-topology

# Emit the complete machine-readable report.
pnpm bench:query-topology -- --report-json /tmp/spaghetti-ipc-topology.json

# Exercise a different scratch corpus without the large-response probe.
pnpm bench:query-topology -- \
  --fixture /path/to/.claude \
  --payload-mib 0 \
  --runs 30 \
  --warmup 5
```

The harness copies the requested fixture into a unique temporary directory,
creates separate legacy and observation databases there, disables unrelated
source auto-detection, and removes the scratch directory on ordinary exit. By
default it adds twelve deterministic 1 MiB memory documents to exercise a
bounded large response. `--keep-workdir` is available only for diagnostics.

Frame telemetry is attached at `IpcTransport`, after encoding on send and
before decoding on receive. It reports exact binary-frame sizes without
changing protocol contents. The callback is isolated from request behavior so
diagnostics cannot fail a query. Utility-process memory comes from a typed
control-plane diagnostic request; it does not expose that process to the
renderer or add a canonical query bypass.

## Reference observation

The accepted run used the committed small Claude fixture, 20 measured runs
after 3 warmups, the default `"error handling"` search, and the twelve-document
payload probe. The machine was an Apple M1 Max with 10 logical CPUs and 64 GiB
RAM, running Electron 41.2.0 and its Node 24.14.0 runtime. The observed Rust
database contained 3 projects, 20 sessions, and 300 canonical messages at
commit 413; the search returned 11 hits.

| Workload | p50 ms | p95 ms | p99 ms | received bytes/request |
| --- | ---: | ---: | ---: | ---: |
| Overview | 0.241 | 0.384 | 0.403 | 287.95 |
| Projects, first 50 | 4.963 | 6.239 | 6.360 | 1,795 |
| Sessions, first 50 | 0.656 | 0.771 | 0.838 | 6,869 |
| Messages, first 50 | 1.037 | 1.267 | 1.283 | 37,230 |
| FTS, first 50 | 0.635 | 0.802 | 0.833 | 13,979 |
| Timeline, first 50 | 1.171 | 1.362 | 1.427 | 29,838 |
| 12.6 MiB memory page | 63.247 | 67.389 | 74.245 | 12,593,755 |

Every ordinary workload emitted exactly one request frame and received exactly
one response frame per logical request. Ordinary event-loop p99 stayed at or
below 1.504 ms. The large page contained 12,582,980 of the 16,777,216 allowed
payload bytes; its event-loop p99 was 16.220 ms and maximum was 19.694 ms.

Cold startup through compatibility-service initialization, observation ingest,
port attachment, and framed negotiation took 1,911.995 ms. Negotiation sent
one 111-byte frame and received one 598-byte frame.

The cancellation burst rejected all 100 requests, fulfilled none, and served a
successful recovery search in a total 6.999 ms. It sent 201 frames: 100
requests, 100 cancellations, and the recovery request. Only three responses
arrived before late cancelled responses were suppressed, including the
recovery response. This is expected asynchronous race behavior, not a claim
that the native work never began.

Utility-process RSS changed from 141,705,216 to 189,333,504 bytes during the
matrix, a 47,628,288-byte increase. Heap used decreased by 829,852 bytes and
external memory increased by 117,267 bytes. These are process observations
without forced garbage collection, so they are diagnostic values rather than
allocation counts or leak evidence.

## Phase boundary

This closes the previously unmeasured same-host Electron utility-process
topology. It supports the selected ownership boundary and establishes a
reproducible report format; one host run does not establish release thresholds.

Remaining rollout evidence includes scale-50 and accepted private-corpus runs,
reviewed regression limits, and deeper native queue/SQLite/conversion telemetry.
These are explicitly external rollout inputs rather than Phase 10 code or
ownership blockers.
The first reversible
[renderer DTO read](./011-phase-10-playground-canonical-stats.md) subsequently
landed for canonical statistics. Broader renderer migration, durable
subscription invalidation, promotion of the Rust utility-process owner, and
production retirement of the TypeScript SQLite graph subsequently completed;
see the [Phase 10 closure ledger](./011-phase-10-closure.md).
