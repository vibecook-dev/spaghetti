# RFC 011 playground cold-start profile — 2026-08-15

Status: diagnostic profile complete; no new optimization accepted by this
report

## Objective

Explain why the production playground still reports approximately 3 minutes
30 seconds from a fresh database to readiness after the single-fixture cold
bootstrap reached 121.85 seconds. The investigation must identify
non-overlapping wall-clock stages, separate nested telemetry from serial time,
and avoid accepting a cause or fix without a controlled comparison.

## Executive finding

A fresh production-shaped replay against the playground's current three
auto-detected source roots reached native-host readiness in 206.89 seconds and
service readiness in approximately 207.02 seconds. This reproduces the
reported 3 minute 30 second startup within the expected Electron and utility
process launch overhead.

The additional time is real engine work, not renderer startup:

- Claude ingestion took 115.60 seconds;
- the sequential Codex and Grok adapters added 55.39 seconds;
- bootstrap finalization added 35.87 seconds; and
- catalog/subscription readiness work added only 0.13 seconds.

The earlier 121.85-second reference processed 1,122,456 records from the
frozen single-adapter fixture. The playground replay processed 1,969,824
records across Claude, Codex, and Grok, emitted 3,702,939 facts, and built an
8,253,603,840-byte database. The playground is therefore not executing the
same workload as that reference measurement.

Within the multi-source replay, the sole writer accumulated 105.92 seconds of
physical transaction work and ten checkpoints accumulated 43.13 seconds.
Source read and decode totals were 39.84 and 33.54 seconds, but those producers
overlap the writer and cannot be added to wall time. The evidence continues to
identify database writes, synchronization, and finalization as the critical
path rather than parsing alone.

## Measurement method

The replay used the release native module and the same production host API and
source ordering used by the playground:

1. open a new temporary database with query bootstrap enabled by the normal
   production threshold;
2. configure `claude-code`, `codex`, and `grok` with the live user roots;
3. record every host progress boundary with its monotonic elapsed time and
   commit sequence;
4. after host readiness, separately time the canonical source-catalog probe
   and the overview read that establishes the subscription cursor;
5. read bounded native owner telemetry through the native query pool; and
6. dispose the owner before inspecting durable error counts through an
   immutable offline SQLite connection.

The temporary 7.7 GiB database was removed after inspection. The bounded raw
telemetry snapshot was retained for the session at
`/private/tmp/spaghetti-playground-cold-profile.json`; the measurements needed
for future comparisons are reproduced in this report.

### Important limitation

This was one production-shaped replay over live source roots, not a frozen
treatment-control-treatment experiment. Active agent files could change while
the replay was running. The measured stage boundaries are exact for this run,
and the workload/output counts are exact snapshots, but proposed causal fixes
below remain hypotheses until they pass a frozen A-B-A or T-C-T gate.

The existing playground database corroborated the scale: it contained
1,313,925 messages, 2,374,765 durable facts, and 37,770 commits. The replay,
performed shortly afterward while sources remained live, contained 1,314,008
messages, 2,374,972 durable facts, and 37,771 commits.

## Product readiness stages

There are seven non-overlapping stages after the observation service starts its
clock:

| Stage | Start | End | Cost | Share of service readiness |
| --- | ---: | ---: | ---: | ---: |
| Preflight threshold scan and owner open | 0.000 s | 0.041 s | 0.041 s | <0.1% |
| Claude ingestion | 0.041 s | 115.640 s | 115.599 s | 55.8% |
| Codex ingestion | 115.640 s | 149.855 s | 34.215 s | 16.5% |
| Grok ingestion | 149.855 s | 171.025 s | 21.170 s | 10.2% |
| Bootstrap finalization | 171.025 s | 206.891 s | 35.866 s | 17.3% |
| Canonical source-catalog probe | 206.891 s | 207.018 s | 0.127 s | <0.1% |
| Subscription cursor setup | 207.018 s | 207.020 s | 0.002 s | <0.1% |

Native host readiness was 206.8906 seconds. The catalog probe took 127.3
milliseconds and the overview/subscription-cursor read took 1.5 milliseconds,
placing service readiness at approximately 207.0195 seconds. A later stats
query brought the diagnostic harness total to 208.06 seconds, but that query is
not part of playground startup.

The user's approximately 210-second observation leaves about 3 seconds for
Electron launch, the utility-process fork, IPC setup, and renderer timing. That
outer overhead is small relative to the engine stages and is not the current
bottleneck.

## Why the 121.85-second reference did not predict playground startup

| Measurement | Frozen reference | Playground replay | Difference |
| --- | ---: | ---: | ---: |
| Wall time | 121.85 s | 206.89 s host ready | +85.04 s |
| Source records read | 1,122,456 | 1,969,824 | +847,368 (+75.5%) |
| Facts emitted | 2,604,162 | 3,702,939 | +1,098,777 (+42.2%) |
| Physical transaction time | 67.82 s | 105.92 s | +38.10 s (+56.2%) |
| Bootstrap finalization | 25.62 s | 35.85 s | +10.23 s (+39.9%) |
| Allocated database | 6,422,183,936 B | 8,253,603,840 B | +28.5% |

An accounting of the wall-time difference is consistent with the stage
profile:

- the previous ingest body was approximately 96.24 seconds after subtracting
  its 25.62-second finalization;
- current Claude ingestion was 115.60 seconds, approximately 19.36 seconds
  more than that body;
- Codex and Grok added 55.39 seconds; and
- larger-database finalization added approximately 10.23 seconds.

Those values account for essentially the full 85.04-second difference. This is
an accounting result across different corpora, not a causal A/B claim about the
19.36-second primary-source delta.

## Adapter-stage evidence

| Adapter | Wall stage | Records read | Payload bytes | Facts emitted | Durable facts | Durable record errors | Source read | Decode |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Claude | 115.60 s | 1,123,368 | 3,179,093,541 | 2,606,176 | 1,513,486 | 6 | 32.46 s | 27.76 s |
| Codex | 34.22 s | 426,739 | 2,017,215,567 | 664,105 | 441,421 | 58,560 | 6.46 s | 4.52 s |
| Grok | 21.17 s | 419,717 | 77,717,612 | 432,658 | 420,065 | 398,745 | 0.92 s | 1.26 s |

The per-adapter source read and decode columns are accumulated operation
durations inside a pipelined stage. They expose where source CPU/I/O work
exists but do not subtract cleanly from adapter wall time.

The largest source streams by accumulated read plus decode time were:

| Adapter/stream | Records | Source read | Decode |
| --- | ---: | ---: | ---: |
| Claude subagent transcripts | 429,382 | 13.12 s | 13.48 s |
| Claude session transcripts | 663,308 | 5.85 s | 14.06 s |
| Codex rollout sessions | 426,739 | 6.46 s | 4.52 s |
| Claude file-history blobs | 22,256 | 9.68 s | 0.12 s |
| Claude subagent metadata | 2,533 | 1.46 s | 0.02 s |
| Claude persisted tool results | 1,600 | 1.21 s | 0.05 s |

## Writer critical path

The writer committed 37,771 logical commits in 1,621 physical transactions,
with zero failed commits and 6,543,287 SQLite row changes. Its physical
transaction time decomposed as follows:

| Writer work | Accumulated time |
| --- | ---: |
| Physical SQLite commit | 32.93 s |
| Canonical projection | 30.78 s |
| Commit/fact preparation | 21.46 s |
| Runtime projection | 14.45 s |
| Usage projection | 4.11 s |
| Cursor/catalog, change log, and disk reserve | 2.09 s |
| **Physical transaction total** | **105.92 s** |

The five largest nested projector counters were:

| Projector | Accumulated time |
| --- | ---: |
| History and fact storage | 27.61 s |
| Fact storage alone | 12.42 s |
| Delegation | 10.17 s |
| Delegation assertion/projection work | 7.90 s |
| Canonical-message storage | 7.53 s |

Checkpoint telemetry recorded ten successful attempts, zero reader-blocked
attempts, zero failures, a zero-frame final WAL, and 43.13 seconds accumulated
checkpoint time. Approximately 4.20 seconds of that total belongs to the
pre/post-finalization checkpoints, leaving about 38.93 seconds in the ingest
period.

The reported 4,927 seconds of aggregate queue wait is deliberately excluded
from wall attribution. It sums the wait observed independently by 37,771
logical commits, many of which wait concurrently inside grouped writer work;
it is not 4,927 seconds of process wall time.

## Bootstrap finalization

The 35.85-second native finalization counter matches the 35.87-second host
boundary:

| Finalization phase | Cost |
| --- | ---: |
| Foreign-key audit | 10.76 s |
| FTS rebuild | 5.89 s |
| FTS integrity audit | 3.55 s |
| Post-finalization checkpoint | 3.21 s |
| Session-activity message index | 2.93 s |
| Run-activity message index | 2.76 s |
| `PRAGMA optimize` | 2.08 s |
| Message-block kind index | 1.20 s |
| Pre-finalization checkpoint | 0.99 s |
| Artifact rebuild | 0.91 s |
| Remaining indexes, trigger/configuration work, and readiness publish | 1.57 s |
| **Total** | **35.85 s** |

The clean-bootstrap optimization correctly removed `quick_check`; it did not
and must not remove the mandatory foreign-key and FTS semantic audits. The
larger multi-source database makes those retained audits and index builds more
expensive than the frozen reference.

## Durable unknown-record diagnostics

The fresh replay persisted 457,311 `record_permanent` source-record error rows:

| Adapter | Rows | Share of that adapter's read records |
| --- | ---: | ---: |
| Grok | 398,745 | 95.0% |
| Codex | 58,560 | 13.7% |
| Claude | 6 | <0.1% |

The most common classifications were:

- 360,754 Grok `phase_changed` events;
- approximately 9,266 each of Grok `tool_started`, `tool_completed`,
  `permission_requested`, and `permission_resolved` events;
- 44,506 Codex `item_completed` events;
- 8,639 Codex `patch_apply_end` events; and
- smaller Codex world-state, compaction, command-end, MCP, and search-end
  variants.

These rows intentionally retained no raw payload bytes, but their error-message
text alone occupied 28,461,163 bytes before table/index overhead. More
importantly, each row participates in durable transaction and index work.

Grok is the sharpest signal: source read plus decode accumulated only 2.18
seconds, while the adapter stage took 21.17 seconds and persisted 398,745 error
rows. This makes per-record unknown-diagnostic materialization a strong next
hypothesis. The current run does not prove how many seconds those rows caused,
because it did not include a same-corpus control that retained equivalent
diagnostic semantics in a compact form.

## Architecture findings

### Sources are deliberately serial

`openObservationHost` awaits `startObservation` for each configured source
inside one loop. Consequently, Claude, Codex, and Grok wall times add directly.
This guarantees simple sole-writer lifecycle ordering but leaves source read
and decode parallelism unused across adapters.

Parallel adapter startup is not yet an accepted fix. The sole writer already
owns most of the critical path, so parallel producers may only increase queue
pressure unless a controlled run shows that source/decode overlap hides useful
wall time without regressing memory, live latency, or deterministic readiness.

### Finalization is invisible in the playground progress model

The host emits a `finalizing` stage every second while
`completeQueryBootstrap` runs. `RustObservationService.emitHostProgress`
handles opening, adapter scanning, adapter ready, and ready, but has no
`finalizing` case. The renderer therefore receives no truthful phase change for
the complete 35.87-second finalization tail.

This is an observability defect, not the cause of the latency. It should be
fixed independently so future profiles and users can distinguish ingest from
index/audit finalization.

## Evidence-gated next experiments

No performance change should be accepted directly from this single replay.
The next work should use a frozen three-source corpus and the same-binary
treatment-control-treatment shape.

### E1 — compact repeated unknown diagnostics

Test a bounded durable representation that preserves:

- adapter, stream, object/generation, error class, and native unknown kind;
- exact occurrence count and first/last provenance;
- a bounded first/last example rather than one row per identical unknown
  event; and
- replacement, retry, audit, and restart behavior.

Measure Grok and Codex stage wall time, writer transaction/commit time, row
changes, checkpoint time, database size, canonical table digests, source-error
counts, and restart convergence. Suppressing diagnostics without an equivalent
audit contract is not a valid treatment.

### E2 — isolate checkpoint policy on the three-source corpus

The ten successful checkpoints accumulated 43.13 seconds. Compare the current
policy with bounded bootstrap-only cadence variants while retaining crash
recovery, disk reserve, zero final WAL, and the established live p99 gates.
Earlier large-WAL experiments regressed smaller corpora, so this must remain a
corpus-shaped controlled test rather than a global pragma change.

### E3 — isolate finalization's remaining scans

The foreign-key audit is the largest remaining finalization phase at 10.76
seconds. Any replacement must prove equivalent whole-database referential
integrity after deferred enforcement and fault injection. FTS rebuild and
integrity are separate semantic gates and must remain independently measured.

### E4 — evaluate concurrent adapter producers only after write reduction

Once E1/E2 establish writer headroom, compare serial and concurrent adapter
read/decode scheduling with the same sole writer. Require bounded queues/RSS,
identical durable outputs and commit convergence, no source starvation, and no
regression in finalization or live latency.

## Current conclusion

The 3 minute 30 second playground startup is now accounted for. It is not a
mystery renderer delay and it does not contradict the 121.85-second frozen
reference. The playground performs a 75.5% larger, three-adapter record load,
waits for those adapters serially, and finalizes a 28.5% larger database.

The largest proven buckets are writer transactions, checkpoints, and mandatory
finalization. The highest-leverage new hypothesis is compacting the 457,311
durable repeated unknown-record diagnostics, especially Grok's 398,745 rows.
That hypothesis remains unaccepted until a frozen multi-source controlled run
demonstrates a causal wall-time win with equivalent audit and convergence
semantics.
