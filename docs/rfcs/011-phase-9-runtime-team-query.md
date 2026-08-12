# RFC 011 Phase 9: runtime and teams query pack

Status: Rust runtime/team shadow slice implemented on 2026-08-12

This slice exposes the durable runtime-state, native presence, team
configuration, membership, and inbox projections through the persistent Rust
query pool. It remains staged on the isolated Claude observation shadow before
production client cutover. TypeScript does not open the shadow database, join
runtime tables, reduce evidence, or assemble team/inbox rows.

RFC 011 lists runtime snapshots and teams sixth in the Phase 9 port order. This
record closes that independently testable query slice; search, timeline,
subagent/workflow, detail, and statistics packs remain open. It does not claim
the Phase 9 exit gate.

## Query contract

The persistent engine and shadow SDK expose five asynchronous operations:

```text
getRuntimeSnapshot({ projectId?, sessionId?, cursor?, limit? })
listTeams({ cursor?, limit? })
getTeam(teamId)
listTeamInboxes(teamId, { cursor?, limit? })
listTeamInboxMessages(inboxId, { cursor?, limit? })
```

Every operation executes through one read-only/query-only Rust worker, opens
one read transaction, returns contract version `1` plus `atCommitSeq`, and
supports request-scoped cancellation from `AbortSignal` through SQLite's
progress handler. List operations default to 50 rows and reject limits outside
1 through 200.

Opaque IDs are separately versioned for projects, sessions, runs, presences,
teams, members, inboxes, messages, and decisive facts. A caller cannot pass a
cursor or entity ID from another operation by accident. Keyset cursors bind to
the query kind, parent scope, and first-page commit watermark; they expire if a
commit occurs between pages.

The runtime page is one ordered stream containing tagged `run` and `presence`
entries. It does not return independently paged arrays whose snapshots could
drift. Ordering uses durable commit sequence, explicit source time, entity
kind, and stable identity. An unscoped request preserves run or presence rows
that arrived before their transcript session. A project scope excludes those
orphans because no project membership has been proved. When both project and
session IDs are supplied, membership is validated.

Team directories are split by disclosure and cardinality:

- `listTeams` returns configuration and inbox/message counts, not message text;
- `getTeam` returns one bounded configuration/member snapshot (Claude's
  adapter limit is 256 members);
- `listTeamInboxes` pages recipients and conflict/unread counts;
- `listTeamInboxMessages` pages sensitive message bodies in authoritative
  snapshot order.

An inbox-only team remains listed with no `config`. Missing lead sessions,
recipients, and senders are exposed with explicit `*Present` flags instead of
causing the source evidence to disappear.

## Durable evidence and explicit non-claims

Run rows expose the materialized observed state and decisive evidence kind,
strength, optional native state, qualified source time, observation time,
source object, and commit sequence. Presence rows expose the native registry
document and deterministic conflict status/counts.

The query pack deliberately does not:

- probe a PID or claim that a registry object means the process is alive;
- persist or return `LikelyActive`, `Stale`, `ProcessMissing`, or another
  transient assessment;
- translate native `working`/`idle` strings into common run lifecycle state;
- infer terminal state from presence deletion, missing team members, a quiet
  inbox, tmux panes, or backend type;
- treat team configuration membership as current execution.

Those boundaries follow the Phase 5 reducers. Queries report what the current
canonical store proves and retain conflicts rather than resolving them in the
presentation layer.

## Legacy parity and accepted differences

The Claude end-to-end shadow fixture is written in the same native shapes read
by the TypeScript oracle. The Rust queries preserve:

- every active-session registry field except host PID liveness;
- team directory/config fields and ordered membership;
- inbox recipient membership and native message order;
- native message IDs/version/kind, content, timestamps, color, and read state.

Accepted semantic differences are intentional:

- canonical presence includes on-disk registry entries without calling
  `kill(0)`, while legacy `listActiveSessionsFromDir()` defaults to filtering
  by current host liveness;
- canonical teams expose provenance, conflicts, opaque identities, and orphan
  relations that the legacy `getTeams()` object cannot express;
- canonical timestamps are ISO qualified values, while legacy team creation
  and join times are epoch milliseconds;
- the canonical API is paginated and snapshot-bound, while legacy
  `getTeams()` returns an unbounded nested object.

The shadow fixture tests direct field equality only where meanings match and
asserts the accepted differences independently.

## Purity, cancellation, and performance evidence

Projection-shaped Rust tests cover run evidence, presence-before-history,
conflicts, cross-kind runtime pages, inbox-only teams, unresolved leads,
missing senders/recipients, member decoding, unread counts, message pages, and
cross-scope cursor rejection. The shared purity test runs empty runtime/team
queries while an independent connection verifies that `PRAGMA data_version`
does not change.

The schema adds ordering indexes for run/presence commit pages, native team
identity, and inbox recipient pages. Existing member and message indexes cover
their authoritative ordinals. The additive indexes are mirrored in Rust and
the temporary TypeScript schema authority without changing schema version 31;
same-version initialization reruns `CREATE INDEX IF NOT EXISTS` statements.

## Remaining Phase 9 work

This is a shadow query surface, not production `SpaghettiClient` cutover. The
remaining gates include search/rank merging, timeline/facets,
subagent/workflow, details/statistics, large-corpus latency and boundary-size
benchmarks, shared IPC/domain DTO generation, and Phase 10 production client
migration. Until then, the legacy TypeScript query service remains the
production read owner and the Rust observation database remains isolated.
