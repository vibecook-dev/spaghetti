# RFC 011 Phase 5: utility-process observation shadow

Status: opt-in isolated shadow host implemented on 2026-08-12

This slice moves the native Claude observation supervisor into the real
long-lived Electron utility-process lifecycle without claiming the production
writer cutover. It establishes the explicit shadow/parity boundary required by
the RFC before legacy watcher shutdown.

## Ownership seam

The current production SDK still reads compatibility tables (`projects`,
`sessions`, `messages`, and related tables) through its process-owned
TypeScript SQLite service. RFC 011 observation commits instead materialize
`canonical_*` projections. Pointing both runtimes at the production database
would therefore create two writers while leaving existing reads unable to see
the new projections.

Shadow mode avoids that invalid intermediate state:

```text
Claude source root
  ├─ legacy TypeScript live owner -> production compatibility DB -> current UI
  └─ Rust observation supervisor -> isolated RFC 011 shadow DB -> diagnostics
```

`openClaudeObservationShadow()` owns one persistent Rust engine, registers
watchers before its initial scan, exposes typed health/overview/refresh calls,
and disposes the supervisor and owner lock deterministically. It canonicalizes
existing path ancestors and refuses collisions between either database and
the other's SQLite WAL, journal, owner-lock, or owner-metadata artifacts. Its
default database is a sibling such as
`spaghetti-rs.observation-shadow.db`; it never opens the production path.

## Host opt-in and diagnostics

The playground utility process enables the shadow only when:

```bash
SPAGHETTI_OBSERVATION_SHADOW=1
```

`SPAGHETTI_OBSERVATION_SHADOW_DB_PATH` optionally selects a different isolated
database. `SPAGHETTI_ROOT_DIR` selects the Claude root for both legacy and
shadow paths; otherwise the SDK default is used.

Production initialization remains authoritative. Shadow startup follows
legacy readiness, and a shadow failure is reported without failing or
restarting the production service. The typed
`getObservationShadowStatus()` IPC/RPC operation returns disabled, starting,
running, degraded, failed, or stopped state; engine health; owner/watcher
status; the durable commit watermark; canonical history counts; and a
Claude-scoped history parity report.

Parity deliberately compares:

- canonical sessions to Claude compatibility sessions;
- canonical messages to Claude parent messages plus raw subagent messages.

Derived subagent timeline rows and non-Claude sources are excluded. The report
preserves signed deltas instead of treating them as host failure. For example,
session-index metadata must not manufacture canonical transcript history, so
temporary or accepted semantic differences need review rather than an
automatic crash.

## Restart and shutdown behavior

The shadow database is durable. Reopening after disposal reacquires the owner
lock, resumes source identities/cursors, and retains canonical counts without
duplicating history. Utility shutdown aborts in-progress shadow startup and
awaits deterministic engine disposal. Production corruption recovery rebuilds
only the production compatibility database; it neither wipes nor forks the
independent shadow owner.

## Evidence and remaining cutover gate

Tests prove path/sidecar isolation, symlink-alias rejection, initial canonical
observation, watched append plus explicit refresh, exact fixture history
parity, concurrent idempotent disposal, and durable reopen. The persistent
engine overview test separately proves canonical counts are visible while
legacy compatibility counts stay zero.

This is not Phase 8 sole-writer cutover. The first typed Rust project/session
query pack is now recorded in
[the Phase 9 query record](./011-phase-9-project-session-query.md); the other
UI read packs remain gates. Only after those queries can serve the production
surface may a feature flag select exactly one complete writer mode:
stop and close the legacy owner first, then open the Rust engine on the chosen
production database. Independent legacy and Rust writers against that database
remain forbidden.
