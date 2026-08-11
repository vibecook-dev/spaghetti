# RFC 011 Phase 1: persistent engine shell

Status: first vertical slice implemented on 2026-08-11

The native addon now exposes a library-first `SpaghettiEngineCore` through an
async N-API/SDK opener:

```ts
import { openSpaghettiEngine } from '@vibecook/spaghetti-sdk';

const engine = await openSpaghettiEngine({
  dbPath: '/absolute/path/spaghetti.db',
  queryWorkers: 2,
  ownerLabel: 'desktop-main',
});

const health = await engine.health();
const overview = await engine.overview();
await engine.dispose();
```

Direct construction is rejected. Opening runs on a libuv worker, and health,
overview, and disposal are Promise-based. `AbortSignal` is accepted by health
and overview; `cancelPendingQueries()` also advances the pool cancellation
epoch so already-queued work is rejected without poisoning later requests.

## Ownership and workers

For `spaghetti.db`, an engine owns:

- `spaghetti.db.owner-lock.sqlite3`: a sidecar SQLite database held in
  `BEGIN EXCLUSIVE`; the OS releases its lock if the process crashes;
- `spaghetti.db.owner.json`: structured diagnostic metadata containing the
  owner id/label, PID, start time, executable, host, database, and versions;
- one dedicated writer thread with one long-lived read/write connection;
- a bounded pool (default 2, maximum 16) whose workers each own one SQLite
  `READ_ONLY` plus `query_only=ON` connection.

A second persistent engine fails with the current owner metadata in its error.
Normal disposal removes metadata only when its owner id matches, stops query
workers, stops the writer, releases the sidecar transaction, and is idempotent
under concurrent callers.

## First typed query

`overview()` returns schema version, the future commit watermark, canonical
project/session/message counts, writer `data_version`/journal mode, and query
confinement flags. `commitSeq` is intentionally zero until Phase 2 adds
durable ingest commits.

The query-purity test runs the real pool connection, verifies `data_version`
does not advance, and proves a DDL write is rejected on that same handle.

## Compatibility boundary

`createSpaghettiService()` remains the explicit legacy compatibility surface
for production ingest and synchronous queries. It is unchanged and does not
open this shell. `openSpaghettiEngine()` is the Phase 1 opt-in surface while
typed query parity is built.

The owner lock is currently cooperative among persistent engine instances;
legacy TypeScript and one-shot native writers do not yet enroll in it. Do not
open the persistent engine and a legacy service against the same database.
Phase 2 will route commits through the owned writer before any production
cutover. This slice intentionally does not add source drivers, durable cursors,
ingest commits, projections, or an outbox.

## Evidence

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- --deny warnings
node --import tsx --test --test-force-exit \
  packages/sdk/src/__tests__/persistent-engine.test.ts
pnpm test:ingest-diff
pnpm test:ingest-diff:medium
pnpm test:ingest-diff:codex
pnpm test:ingest-diff:grok
```

The Rust suite covers exclusive ownership, metadata, cancellation races,
query purity, worker health, shutdown, finalizer fallback, and clean reopen.
The Node suite covers the generated N-API class and SDK Promise contract. All
four existing Rust-versus-TypeScript fixture oracles remain at zero diffs.
