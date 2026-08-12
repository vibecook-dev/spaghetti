# RFC 011 Phase 9: canonical usage query pack

Status: Rust usage totals/activity shadow slice implemented on 2026-08-12

This slice exposes RFC 011 usage contributions and materialized totals through
the persistent Rust query pool. It is intentionally staged on the isolated
Claude observation shadow before production client cutover. TypeScript does
not open the shadow database, read canonical usage tables, aggregate token
rows, or assign quality.

RFC 011 lists usage activity and totals fifth in the Phase 9 port order. This
record closes that independently testable query slice. Canonical FTS search is
recorded separately; timeline and subagent/workflow packs remain open.
Runtime/team, capability details, and core details/statistics are recorded
separately as complete slices. This does not claim the Phase 9 exit gate.

## Query contract

The persistent engine and shadow SDK expose two asynchronous operations:

```text
getUsage({ projectId, sessionId? })
getUsageActivity({ projectId, sessionId?, from, to })
```

Both operations:

- cross N-API once per logical report and execute on the bounded persistent
  Rust read pool;
- use a read-only/query-only SQLite connection;
- run every statement for one response inside one read transaction;
- return query contract version `1` and `atCommitSeq` from that snapshot;
- accept opaque canonical project/session identities returned by the history
  query pack;
- reject malformed identities and a session that does not belong to the
  requested project;
- return an empty `unavailable` aggregate for a valid project identity with no
  usage evidence;
- preserve exact and estimated values in separate buckets and derive `mixed`
  only when both are present;
- expose contribution and distinct-session counts without treating either as
  transcript message counts;
- expose source/observation/commit evidence instead of presenting ingest time
  as native event time.

`getUsageActivity` accepts inclusive, Gregorian `YYYY-MM-DD` bounds. It rejects
impossible dates, reversed ranges, and ranges longer than 366 days. Daily
grouping uses the first ten characters of a structurally and calendrically
valid source timestamp. Missing, short, malformed, impossible, and year-zero
source dates are returned in the separate `untimed` aggregate; they are never
assigned to a fabricated day or silently discarded. The requested-range
aggregate excludes `untimed` by definition.

## Usage semantics

Every aggregate returns:

- `exact`, `estimated`, and `combined` token values;
- input, output, cache-creation, and cache-read components;
- `componentTotalTokens`, the checked arithmetic sum of those four preserved
  components;
- `exact`, `estimated`, `mixed`, or `unavailable` quality;
- exact/estimated/combined contribution counts;
- distinct contributing session count.

`componentTotalTokens` is deliberately not named or interpreted as a provider
billing total. Claude cache fields are additive, while another adapter may
report cached input as a subset of input. The query pack preserves components
and quality so provider-aware presentation can normalize without corrupting
the common read model.

Coverage rows retain the dimensions that determine whether values may be
combined honestly:

```text
scope
accounting
valueQuality
qualityBucket
model
sourceTimeQuality
contributionCount
token components
```

The current projector accepts `delta` contributions only; the public
accounting field remains explicit so cumulative/snapshot reducer support can
be added without reinterpreting existing rows. All-time numeric totals read
the O(session-count) `usage_totals` materialization, while coverage and
evidence metadata read canonical contributions in the same snapshot. Activity
uses contributions because date/coverage dimensions cannot be reconstructed
truthfully from the session total alone.

## Legacy parity and accepted differences

`compareClaudeObservationUsage()` is a source-specific migration oracle. For
each committed Claude fixture project it compares:

- the four exact native token components for all-time totals;
- the same exact components for every activity day;
- absence of estimated and untimed Claude contributions.

It also reports missing, unexpected, and field-mismatched dates. The committed
small Claude corpus passes this normalized differential for every compatibility
project.

Two differences are accepted and named in the parity report:

- `canonical_component_total_is_not_provider_billing_total`: the canonical
  total is a transparent component sum, while the legacy total is
  source-normalized;
- `canonical_contribution_count_is_not_legacy_message_count`: the canonical
  count measures nonzero usage facts, while the legacy daily count measures
  transcript rows, including rows with no usage.
- `canonical_session_count_is_not_legacy_transcript_session_count`: canonical
  daily sessions have usage evidence, while the legacy field counts sessions
  with any transcript row;
- `canonical_days_require_usage_evidence`: a legacy transcript-only day with
  four zero token components is accepted as absent from canonical activity.

The normalizer does not weaken equality by comparing those non-equivalent
fields.

## Query purity, cancellation, and performance evidence

The query-purity test executes totals and activity alongside overview and
history queries while an independent SQLite connection verifies that
`PRAGMA data_version` does not change. Empty reads do not create or repair a
projection, and the actual query worker still rejects a write probe.

Usage requests support both cancellation layers:

- the engine cancellation epoch rejects already queued work while allowing
  requests submitted under the new epoch;
- a request-scoped token is wired from N-API `AbortSignal` into SQLite's
  progress handler, interrupting an already-running statement without
  cancelling unrelated queries.

Tests deterministically cover queued cancellation and prove a running
recursive SQLite statement is interrupted before completion.

Schema DDL now includes
`idx_usage_contributions_session_time(session_key, source_time, fact_id)`.
An `EXPLAIN QUERY PLAN` test requires SQLite to seek usage activity by canonical
session and source time. The additive index is mirrored in the Rust and
temporary TypeScript schema authorities. Schema version 31 does not change
because same-version initialization reruns all `CREATE INDEX IF NOT EXISTS`
statements.

## Remaining Phase 9 work

This is a shadow query surface, not production `SpaghettiClient` cutover. The
remaining gates include:

- timeline/facet packs (canonical FTS search is recorded separately);
- subagent/workflow queries (the capability-detail pack is recorded
  separately);
- large-corpus latency, boundary-size, and concurrent-ingest benchmarks;
- IPC/domain DTO sharing beyond the current N-API shadow seam;
- production client migration and retirement of TypeScript SQLite query
  ownership in Phase 10.

Until those gates pass, the legacy TypeScript query service remains the
production read owner and the Rust observation database remains isolated.
