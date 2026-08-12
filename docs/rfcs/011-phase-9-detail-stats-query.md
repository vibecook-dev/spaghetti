# RFC 011 Phase 9: details, statistics, and simple lookups

Status: Rust core detail/statistics shadow slice implemented on 2026-08-12

This slice exposes exact session and run lookups, bounded canonical message
pages, source inventory, and canonical database statistics through the
persistent Rust query pool. It remains staged on the isolated Claude
observation shadow before production client cutover. TypeScript does not open
the shadow database, assemble rows, or calculate canonical counts.

RFC 011 lists details, statistics, and simple lookups seventh in the Phase 9
port order. This record closes the core history/runtime/catalog portion of
that item. The adjacent capability-detail, canonical FTS, timeline, and
delegation/workflow surfaces are recorded separately. It does not claim the
Phase 9 exit gate.

## Query contract

The persistent engine and shadow SDK expose five asynchronous operations:

```text
getSession(sessionId)
getMessages({ projectId, sessionId, cursor?, limit? })
getRunState(runId)
listSources({ cursor?, limit? })
getStats()
```

Every operation executes through one bounded, read-only/query-only Rust
worker, opens one read transaction, returns contract version `1` plus
`atCommitSeq`, and supports request-scoped cancellation from `AbortSignal`
through SQLite's progress handler. Message and source pages default to 50 rows
and reject limits outside 1 through 200.

Project, session, message, run, source-instance, and decisive-fact identities
are opaque, typed, and versioned. Malformed or cross-kind identities are
rejected before entering the worker queue. Exact session/run lookups return an
absent value for a well-formed unknown identity. Message requests validate
that the session currently belongs to the requested project.

Message cursors bind the project, session, ordering key, contract version, and
first-page commit watermark. Source cursors bind adapter ID, source-instance
ID, contract version, and watermark. A cursor expires after any later durable
commit rather than continuing through a drifting snapshot.

## Detail semantics and payload bounds

`getSession` returns the transcript-backed canonical row, separately sourced
session-index enrichment, decisive provenance, and counts for messages, runs,
presences, task collections, artifacts, workflows, persisted tool results,
and project-memory documents. A count indicates observed canonical rows; it
does not imply that the corresponding capability detail query has shipped.

`getMessages` returns common message fields and two explicit JSON values:

- `content` is the adapter-neutral content-block projection;
- `nativePayload` is the lossless source record used by legacy/raw consumers.

Timed rows sort by qualified source time and stable message identity. Rows
without a valid source time follow timed rows in stable identity order. That
is a canonical chronology, not an assertion that every adapter's physical
record order is interchangeable with time order.

Rust enforces both the page row limit and a 16 MiB limit over the UTF-8 bytes
of canonical content JSON plus native payload JSON. The response reports
`payloadBytes` and `payloadByteLimit`. Scalar metadata remains bounded by the
ingest contract. A single row that cannot fit is rejected rather than
silently truncated.

`getRunState` reuses the runtime query pack's evidence DTO and non-claims: it
reports durable observed state and registry evidence without probing PIDs or
inventing liveness.

## Source inventory and canonical statistics

`listSources` returns one row per configured source instance, preserving the
adapter ID and contract version together with stream, availability, object,
quarantine, fact, and committed-ingest counts. It does not collapse multiple
roots of one adapter into a single source string.

`getStats` reports source catalog sizes, committed facts, searchable canonical
messages, projection readiness, stream states, canonical entity counts, and
allocated SQLite pages. Its entity names are part of the versioned contract.
Compatibility tables (`projects`, `sessions`, `messages`, and their derived
caches) are intentionally excluded, even while Phase 10 has not retired them.

The legacy `getStats()` object therefore is not an equality oracle:
`totalSegments`, `source_files`, compatibility FTS rows, and file size answer
different questions. Direct parity is asserted for native source identity,
lossless parent-message payloads, and session/run facts; canonical stats are
tested against the canonical schema itself.

## Purity, cancellation, and performance evidence

Projection-shaped Rust tests cover exact/null lookups, malformed identities,
timed-before-untimed ordering, canonical/native JSON decoding, row and byte
bounds, project/session membership, cursor scope rejection, cursor watermark
expiry, source pagination, canonical-only counts, compatibility-row
exclusion, and database page arithmetic. Runtime tests cover exact run lookup
using the same reducer DTO as runtime pages.

The Claude shadow lifecycle test compares `nativePayload` directly with the
committed TypeScript parent-message oracle, compares the source adapter list,
and exercises all five N-API operations through the SDK facade. The common
query actor supplies pre-queue and in-flight cancellation.

Schema DDL now includes
`idx_fact_records_source_instance(source_instance_id, fact_id)` so source
inventory can aggregate the fact audit store by source without a table scan.
The additive index is mirrored in Rust and the temporary TypeScript schema
authority without changing schema version 31; same-version initialization
reruns `CREATE INDEX IF NOT EXISTS` statements.

## Remaining cutover work

The other query packs and consolidated N-API conformance, scaled-history,
payload-boundary, cancellation, and concurrent-refresh evidence are recorded
in the [Phase 9 query gate](./011-phase-9-query-conformance-benchmark.md).

This remains a shadow query surface until Phase 10. Remaining work includes
shared IPC/domain DTOs and topology benchmarks, operational scale-50 and
private-corpus soak evidence, production client migration, and retirement of
TypeScript SQLite query ownership. Until that cutover, the legacy service
remains the production read owner and the Rust observation database remains
isolated.
