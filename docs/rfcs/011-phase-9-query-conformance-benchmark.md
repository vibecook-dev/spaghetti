# RFC 011 Phase 9: query conformance and N-API benchmark gate

Status: Phase 9 N-API shadow exit satisfied on 2026-08-12

> Historical boundary: pre-cutover ownership language below records the state
> when this gate ran. Phase 10 subsequently cut every production consumer over;
> see the [Phase 10 closure ledger](./011-phase-10-closure.md).

This record closes the correctness and benchmark work shared by the Phase 9
query packs. It does not perform the Phase 10 production-client cutover or
claim an IPC result before that transport exists.

## Reproducible harness

`scripts/bench-queries.ts` is the canonical query gate:

```bash
# Complete-surface conformance on the committed capability-rich fixture.
pnpm test:query-conformance

# Conformance plus the TypeScript-versus-Rust N-API benchmark matrix.
pnpm bench:queries --report-json /tmp/spaghetti-query.json

# A deterministic scaled history corpus.
tmp_dir="$(mktemp -d /tmp/spaghetti-query-scale10.XXXXXX)"
node scripts/generate-medium-fixture.mjs --out "$tmp_dir" --scale 10
pnpm bench:queries \
  --fixture "$tmp_dir/.claude" \
  --payload-mib 0 \
  --runs 20 \
  --warmup 3 \
  --report-json /tmp/spaghetti-query-scale10.json
```

The harness never writes the requested fixture. It copies the corpus into a
unique temporary directory, creates independent TypeScript-oracle and Rust
observation databases there, and removes that directory on every ordinary
exit. Real agent data is therefore opt-in and still benchmarked only through
a scratch copy. A `--keep-workdir` diagnostic option makes the exact scratch
path explicit when post-run inspection is needed.

By default, the scratch copy gains two deterministic probes that are not
committed as product fixture data:

- one valid presence record plus one team/config/inbox pair, so runtime,
  run-state, team detail, inbox, and inbox-message operations are all
  exercised with non-empty results;
- twelve valid 1 MiB project-memory topic documents, so a single 16 MiB
  native page must return at least 70% of its payload allowance.

`--payload-mib 0` disables the second probe for scaled history runs. The
complete-surface probe remains enabled. TypeScript compilation, ESLint, and
Prettier coverage for the harness are part of the repository commands.

## Conformance contract

One run executes every method on `ClaudeObservationShadow`, including the
aggregate history comparison, all paged query families, every available
detail lookup, lifecycle snapshots, refresh, and disposal. Runtime discovery
walks every cursor page; the scaled corpus crosses the 200-entry boundary.

The hard assertions cover twelve groups:

1. aggregate plus normalized project/session history parity;
2. session-detail coverage;
3. lossless root-message payload parity;
4. literal FTS exact-total parity and one shared root/delegated score domain;
5. timeline totals, facets, payload bounds, and cursor semantics;
6. delegation identity parity;
7. workflow summary, detail, member, and nested-agent parity;
8. memory, task, plan, persisted-result, and artifact payload contracts;
9. exact usage-component and daily-activity parity;
10. source, stats, runtime, run-state, team, and inbox contracts;
11. read-only/query-only purity across the complete query surface;
12. pre-queue cancellation.

The query-purity assertion snapshots both the commit sequence and SQLite
writer data version before and after the complete read suite. Neither may
change. Every ordinary versioned response must carry contract version `1`
and the same durable commit watermark. Every returned payload counter must
remain under its engine-owned byte limit.

The accepted semantic differences are named in report JSON rather than
normalized away:

- canonical message counts include delegated transcripts;
- equal-timestamp message ties use stable canonical identity rather than
  legacy transcript ordinal;
- timeline facets count canonical envelopes and content blocks rather than
  legacy display rows;
- canonical token totals are additive components rather than provider
  billing totals;
- canonical statistics exclude compatibility-cache rows;
- workflow state, member evidence, and child-run state remain separate.

## Benchmark matrix and metrics

The measured matrix includes warm metadata, project and session aggregation,
message paging, parent-plus-delegated FTS top 50, timeline plus facets and
total, project usage activity, delegation/workflow discovery when present,
bounded detail payloads, deep pagination, ten-reader bursts, cancellation,
and ten readers overlapping a real scratch-corpus refresh.

For both the synchronous TypeScript oracle and asynchronous Rust N-API path,
the JSON report records end-to-end min/p50/p95/p99/max/mean, encoded response
bytes, process heap/RSS deltas, event-loop delay, and API calls per logical
request. Ordinary Rust workloads use one N-API request. Legacy timeline is
reported as two compatibility calls because page and facets are separate.
The ten-reader workload intentionally reports ten calls.

Heap/RSS deltas are process observations without forced garbage collection;
they are diagnostic rather than allocation counts. SQLite time, worker-queue
time, Rust allocation, and conversion sub-timings require native telemetry
that the current surface does not expose. At this Phase 9 boundary IPC was
also unmeasured because no selected endpoint existed, so the harness says so
instead of fabricating those metrics. Phase 10 subsequently recorded the real
[playground utility-process topology](./011-phase-10-playground-ipc-benchmark.md).

## Release-build evidence

The accepted observations below used the optimized `release` N-API addon
0.7.0 on an Apple M1 Max (10 logical CPUs), arm64 macOS, 64 GiB RAM, and Node
26.5.0. They are a same-host baseline, not a cross-machine latency promise.

### Complete capability and payload corpus

The default run used the committed small fixture plus only the scratch probes:

- 3 projects, 20 sessions, and 300 canonical root/delegated messages;
- all twelve conformance groups passed at commit 419;
- non-empty delegation, workflow, workflow-member, memory, task, plan,
  persisted-result, artifact, runtime, run-state, team, inbox, and
  inbox-message reads;
- the memory page returned 12,582,980 of 16,777,216 allowed payload bytes
  (75% utilization) with exact content bytes;
- 100 of 100 cancelled search requests rejected, followed by a successful
  recovery search;
- ten-reader p95 was 3.106 ms alone and 3.930 ms while refresh committed one
  appended message; refresh took 73.647 ms.

Ordinary Rust page p95 values on this small capability corpus ranged from
0.185 ms to 4.392 ms. The intentionally large 12.6 MiB memory response had
p50 70.638 ms and p95 118.931 ms. Its p99 was dominated by one visible
JavaScript GC/scheduler outlier, which is retained in JSON rather than hidden.
The legacy memory result is only the native index document, so that row is a
boundary-cost measurement, not a speedup comparison.

### Scaled history corpus

The deterministic `--scale 10` run used 8,148,015 source bytes, 285 JSONL
files, 284 sessions, and 11,394 canonical messages. It passed all twelve
groups at commit 11,713, including a complete walk of 286 runtime entries,
one team, one inbox, and two inbox messages. Search found 325 exact
`"error handling"` hits.

Selected Rust N-API p50/p95/p99 milliseconds were:

| Workload | p50 | p95 | p99 |
| --- | ---: | ---: | ---: |
| Metadata/statistics | 10.262 | 12.062 | 12.931 |
| Project list | 4.606 | 5.095 | 5.206 |
| Session list | 6.349 | 7.046 | 7.115 |
| Message page 50 | 1.065 | 1.273 | 1.490 |
| FTS top 50, root + delegated | 4.760 | 5.760 | 6.508 |
| Timeline + facets + total | 1.167 | 1.323 | 1.441 |
| Project usage activity | 58.384 | 60.703 | 61.512 |
| Deep keyset message page | 0.217 | 0.346 | 0.493 |

Event-loop p95 stayed between 1.183 ms and 1.565 ms for those ordinary Rust
workloads, supporting the async off-thread execution claim. All ordinary
logical requests crossed N-API once. Deep canonical paging remained keyset
based and substantially avoided the legacy offset cost on this corpus.

The scaled cancellation burst rejected 100 of 100 requests in 1.668 ms and
immediately served a 325-hit recovery search. Ten-reader search p95 was
50.477 ms alone and 66.508 ms while one appended message was reconciled and
committed, a 1.318x ratio. The refresh completed in 164.503 ms, advanced the
watermark exactly once, exposed the marker exactly once, and left a bounded
4,618,552-byte WAL observation.

The scaled usage report is the clearest optimization lead: roughly 60 ms p95
for one project on this synthetic corpus. It is correct and asynchronous, but
should be profiled before Phase 10 decides permanent worker count and rollout
thresholds. No regression limit is inferred from one host; the committed
harness emits JSON so reviewed release baselines can establish that policy.

The fixture generator documents scale 50 as its CI-sized target. That run was
attempted but did not complete inside the available execution window, so this
record does not mislabel scale 10 as the final CI/real-corpus soak. Scale 10
is the accepted reproducible Phase 9 query baseline; scale 50 and a private
real-data soak remain operational follow-up evidence before broad rollout.

## Phase boundary

All named production query capabilities now exist in Rust, the consolidated
N-API conformance suite passes, and the suite proves that queries do not
mutate the database. This satisfies the Phase 9 exit for the in-process N-API
shadow topology.

The remaining work is deliberately Phase 10 or rollout hardening:

- carry the now-defined shared transport DTOs and measured selected IPC
  topology into product-consumer migration;
- introduce and migrate the asynchronous `SpaghettiClient` consumers;
- retire TypeScript SQLite read ownership only after the client cutover;
- run scale-50 and accepted private-real-corpus soak reports;
- expose native queue/SQLite/allocation/conversion telemetry and establish
  reviewed regression thresholds, with special attention to usage activity.

Until Phase 10 cutover, the legacy TypeScript query service remains the
production read owner and the Rust observation database remains isolated.
