# RFC 011 Phase 9: canonical project/session query pack

Status: first Rust canonical query-parity slice implemented on 2026-08-12

This slice exposes project aggregation and per-project session summaries from
the RFC 011 canonical projections. It is intentionally available through the
isolated observation shadow before any production client cutover. TypeScript
does not open the shadow database, know canonical table names, execute SQL, or
merge and sort native rows.

RFC 011 lists project/session summaries fourth in the Phase 9 port order. This
slice starts that independently testable pack while search, timeline, and
subagent/workflow query packs remain open. It does not claim the Phase 9 exit
gate or reorder the remaining production cutover.

## Query contract

The persistent engine and SDK expose two asynchronous operations:

```text
listHistoryProjects({ limit?, cursor? })
listHistorySessions({ projectId, limit?, cursor? })
```

Both operations:

- execute on the bounded persistent Rust read pool;
- use a read-only/query-only SQLite connection;
- run every statement for one response inside one read transaction;
- return query contract version `1` and `atCommitSeq` from that snapshot;
- default to 50 rows and reject limits outside 1 through 200;
- return opaque, base64url, versioned keyset cursors;
- bind cursors to query kind, project filter where applicable, and the first
  page's commit watermark;
- reject malformed, cross-query, cross-project, unsupported-version, and
  expired cursors instead of silently changing their meaning;
- sort by latest canonical activity evidence descending, then stable binary
  entity identity descending.

The watermark binding makes pagination a fixed committed snapshot contract
without retaining server-side cursor state. If observation commits between
pages, the cursor expires and the client restarts at page one. This avoids
duplicates or gaps caused by continuing a keyset walk over a different
snapshot.

## Canonical semantics

Project identity is an opaque canonical key plus explicit adapter and source
instance identity. Native project keys remain diagnostic/source-owned fields;
they are not assumed globally unique. Project rows can be evidenced by a
canonical transcript, a project-level session index, or a project-memory
document.

Project summaries report:

- transcript-backed canonical session count;
- all canonical transcript message rows, including parent and subagent
  messages;
- project-memory document count and a separate native memory-index flag;
- latest activity value and its source (`message`, `session`, or
  `session_index`);
- optional project-index status, original path, entry/assertion/conflict
  counts, and its own commit sequence;
- the maximum commit sequence contributing to the row.

Session pages contain transcript-backed sessions only. A session-index entry
never manufactures history. Transcript fields (`cwd`, branch, first prompt,
AI title, custom title, message count, and first/last qualified message times)
remain separate from optional index enrichment (`fullPath`, summary, native
message count, created/modified times, branch, sidechain flag, transcript join
state, resolution/conflict state, and assertion counts). The DTO does not
guess a display title, merge native/index counts, repair timestamps, or
convert metadata-only entries into sessions.

The activity sort is explicit evidence precedence when timestamps tie:
message, then session, then session index. Memory documents do not carry an
activity timestamp in the committed projection and therefore do not invent
one.

## Legacy parity and accepted differences

`compareClaudeObservationHistoryQueries()` compares only like-for-like fields
against the TypeScript compatibility oracle:

- native project/session membership;
- transcript-backed session counts;
- canonical all-transcript message counts against legacy parent plus raw
  subagent message counts;
- native memory-index presence;
- session-to-project identity.

Canonical-only projects are accepted only when they have no transcript
session or message rows, which covers truthful index-only and memory-only
evidence. A canonical-only transcript project or session is a parity failure.

The public canonical `messageCount` intentionally includes subagent
transcripts. Legacy project/session list counts are parent-only; treating them
as equivalent without normalization would discard real history. This accepted
difference is named
`canonical_message_count_includes_subagents` in the parity report.

The committed small Claude corpus passes the normalized differential across
all compatibility projects and sessions with no missing canonical history and
no unexpected canonical-only transcript history.

## Query purity and performance evidence

Tests execute overview, project-page, and empty session-page queries while a
separate SQLite handle verifies `PRAGMA data_version` does not change. The
actual query worker also rejects a write probe. Queued project-page work is
deterministically cancelled by the common query cancellation epoch, while
requests submitted under the new epoch succeed.

Project/session counts still aggregate exact canonical identities at query
time. Schema DDL now includes
`idx_canonical_messages_session_activity(session_key, source_time,
message_key, source_time_quality, last_commit_seq)`. An
`EXPLAIN QUERY PLAN` test requires SQLite to use it as a covering index for
message count/time aggregation, preventing list pages from reading
`content_json` or `raw_json` payload blobs. The index is additive and mirrored
in both Rust and temporary TypeScript schema authorities; schema version 31
does not change because same-version initialization reruns all
`CREATE INDEX IF NOT EXISTS` statements.

## Remaining Phase 9 work

This is a shadow query surface, not production `SpaghettiClient` cutover. The
remaining gates include:

- FTS search/rank merging and timeline/facet packs;
- subagent/workflow, usage, runtime/team, detail, and statistics packs;
- real-corpus latency and boundary-size benchmarks;
- IPC/domain DTO sharing beyond the current N-API shadow seam;
- production client migration and retirement of TypeScript SQLite query
  ownership in Phase 10.

Until those gates pass, the legacy TypeScript query service remains the
production read owner and the Rust observation database remains isolated.
