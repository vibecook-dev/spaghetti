# RFC 011 Phase 2: transactional source catalog and durable outbox

Status: transaction-boundary exit gate met on 2026-08-11

Phase 2 establishes the durable commit protocol that later source drivers and
typed fact projectors must use. The implementation is library-first and lives
behind the persistent engine's one writer actor; the low-level commit envelope
is crate-internal and is not an adapter or TypeScript API.

## Schema 17

Schema version 17 adds:

- `source_instances`, `source_streams`, and `source_objects`, including binary
  stable keys, object identities, generations, committed cursors, decoder
  state, observed revisions, and source state;
- `ingest_commits` as the committed sequence authority;
- `projection_versions` with desired/completed versions and explicit
  `ready`, `stale_safe`, `pending`, or `unavailable` readiness;
- `source_record_errors` for durable record diagnostics/quarantine metadata;
- `change_log` as the ordered transactional outbox.

The existing wipe-on-version-change policy still applies: opening a version 16
database through either migration path rebuilds it at version 17. Rust is the
target schema authority. `packages/sdk/src/data/schema.ts` temporarily mirrors
the DDL so the legacy TypeScript oracle remains usable during migration, and
the ingest differential harness now requires all seven RFC 011 tables on both
sides.

## Commit protocol

One internal observation commit executes under `BEGIN IMMEDIATE` on the
persistent writer connection:

1. validate the typed envelope;
2. upsert the source instance and stream and allocate `commit_seq`;
3. compare the stored object generation/cursor with the caller's expectation;
4. reserve a new object's transaction-local identity, then apply common
   canonical, runtime, and usage work with complete provenance ids;
5. advance object generation/cursor and persist decoder state;
6. update projection readiness and record diagnostics;
7. append ordered change rows with `(commit_seq, ordinal)` cursors;
8. set `committed_at` and `fact_count`, then commit;
9. only after SQLite commit may in-memory publication begin.

`ExpectedSourceCursor::Absent` is distinct from an empty cursor. Existing
objects use a compare-and-swap precondition over generation plus cursor. A
retry of a range already committed therefore returns `StaleSourceCursor`
without allocating a second visible commit or duplicating outbox effects.

Adapters will emit typed facts in later phases. They do not implement the
transaction trait or choose projection SQL, readiness rows, or public change
topics. The crate-private common-projection seam is exercised today with real
canonical/runtime/usage fixture writes, which proves those writes share the
same rollback boundary before production fact reducers are connected.

## Durable replay and watermarks

`replay_changes()` executes on the bounded read-only/query-only worker pool.
One SQLite read transaction reads the current committed watermark, oldest
available cursor, and a bounded ordered page, then returns them together.
Pages are strictly after the supplied `(commit_seq, ordinal)`, optionally
filter by up to 64 stable topics, and fetch at most 1,000 changes. A snapshot
consumer can use `ChangeCursor::after_snapshot(at_commit_seq)` to start after
all changes represented by that snapshot.

The low-level replay model is currently a Rust library contract. N-API still
exposes lifecycle, health, and overview only; `overview().commitSeq` now reads
the latest finalized commit. Live subscription delivery, acknowledgement,
retention/pruning, reset-required responses, and the async semantic client are
later migration phases.

## Failure evidence

Deterministic process-like failure is injected at:

- before the transaction;
- after canonical, runtime, and usage projection mutations;
- after the cursor update;
- after outbox insertion;
- immediately before commit;
- immediately after commit;
- before in-memory publication.

Every pre-commit point leaves source catalog, projection fixtures, cursor,
diagnostics, commit row, and outbox unchanged. Both post-commit points leave
all of them durable; retry is rejected by the cursor precondition, and a new
engine/query-pool instance replays the ordered changes from disk.

The focused and compatibility evidence is:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
pnpm typecheck
pnpm lint
pnpm format:check
pnpm build
pnpm test:ingest-diff
pnpm test:ingest-diff:medium
pnpm test:ingest-diff:codex
pnpm test:ingest-diff:grok
```

Phase 3 can now add common append/snapshot drivers against this commit
boundary. It must not introduce an alternate cursor store, pre-advance a
cursor in TypeScript, expose this internal commit envelope, or publish before
the writer returns a durable receipt.
