# RFC 009: Retire the TypeScript Bulk Ingest Engine

**Status:** Superseded and completed through RFC 011
**Created:** 2026-08-07
**Author:** James Yong + Kimi
**Hard dependency:** completed readiness report from [RFC 008 — Rust Bulk Ingest Production Readiness](./008-rust-ingest-production-readiness.md)
**Independent of:** [RFC 007 — Retire the Runtime Bridge](./007-retire-runtime-bridge.md)

> **Supersession note:** RFC 011 completed the broader target: Rust owns cold
> and live source observation, the Spaghetti database, projections, queries,
> and durable delivery. The TypeScript source/storage implementation remains
> only as an isolated repository differential oracle and is absent from
> production and published package graphs. Clauses below that retain a
> TypeScript live writer or rollback owner are historical and no longer apply.

---

## Summary

Retire the TypeScript cold/warm bulk ingest engine after Rust has shipped and soaked as production-ready. Remove engine selection and dual-engine maintenance, convert parity tests into native goldens, and move the default cache to one `spaghetti.db`.

This is a staged cutover, not a single deletion change:

1. Verify eligibility and land deletion seams without changing behavior.
2. Publish deprecations and a dormant, testable database bootstrap.
3. Make omitted/default selection native-required while retaining an explicit TS escape hatch.
4. Enable the single database through a clean native rebuild without adopting, moving, or deleting old per-engine caches.
5. After another release gate, delete TS bulk code and engine-selection surfaces.
6. Finish docs, compatibility cleanup, and optional old-cache cleanup.

The database strategy intentionally does **not** adopt or move `spaghetti-rs.db`. The index is a rebuildable cache, so a clean native rebuild is safer and easier to recover than a donor/WAL migration state machine.

---

## Eligibility

Phase 0 cannot begin until RFC 008's dated readiness report exists and confirms:

- warm reconciliation and upgrade repair shipped;
- project abort/error behavior shipped;
- token-estimation policy settled;
- platform artifacts and glibc floor published;
- native-unavailable diagnostics shipped;
- at least one dual-engine soak release completed;
- no unresolved correctness-class divergence.

If any condition regresses during this RFC, cutover pauses and the issue returns to RFC 008's contract. This RFC cannot waive readiness findings to keep a release date.

---

## Scope

This RFC owns:

- staged removal of TS cold/warm parsing and workers;
- staged removal of engine settings, environment variables, CLI/UI controls, and SDK exports;
- separation of retained TS live-write code from bulk-only code;
- removal of the unused native live-batch route;
- a clean-rebuild bootstrap for the single default database;
- integration of `rebuildIndex()` and corrupt-cache recovery with that bootstrap identity;
- conversion of cross-engine diffs into deterministic native golden tests;
- release, rollback, and old-cache cleanup policy.

---

## Non-goals

1. Do not fix Rust ingest correctness here; RFC 008 owns it.
2. Do not move the TypeScript live writer or filesystem watchers into Rust.
3. Do not change the query path, TUI navigation, or playground shell beyond engine controls.
4. Do not adopt, rename, checkpoint, or otherwise trust an old engine database as the new cache.
5. Do not automatically delete `spaghetti-rs.db` or `spaghetti-ts.db` during the rollback window.
6. Do not retire runtime bridge/plugins; RFC 007 owns them.
7. Do not add sources or parser capabilities.

---

## Decisions

| ID  | Decision                 | Choice                                                                                                      |
| --- | ------------------------ | ----------------------------------------------------------------------------------------------------------- |
| C1  | Cutover shape            | Staged across published releases, with explicit gates                                                       |
| C2  | Unified DB creation      | Clean Rust rebuild; never adopt an old donor DB                                                             |
| C3  | Old per-engine caches    | Never adopt, move, or auto-delete them; only a deliberately selected legacy engine may update its own cache |
| C4  | Caller-owned `dbPath`    | Never auto-bootstrap or rename it                                                                           |
| C5  | Live writes              | Keep the existing TypeScript `IngestService` path                                                           |
| C6  | Native addon unavailable | Throw actionable `EngineUnavailableError`; never silently fall back after TS removal                        |
| C7  | Compatibility warnings   | One release of warnings after a surface stops working, then remove the warning code                         |

Changing these choices requires editing this table before implementation.

---

## Final architecture

```text
Cold/warm source files
        |
        v
Rust native bulk ingest ---------> spaghetti.db
                                      ^
                                      |
TypeScript live watchers -> TS IngestService
                                      |
                                      v
                              query path (TS)
```

There is one bulk engine and one live writer. “Retire TypeScript ingest” means retiring the TS **bulk** path, not deleting TypeScript SQLite writes.

---

## Retained live-plane contract

The following are retained unless a later RFC replaces the live plane:

- `data/ingest-service.ts` write batching, prepared statements, row handlers, and sink methods used by live updates;
- fingerprint and next-message-index access used by live watchers;
- `sources/*/message-extractor.ts`;
- everything under `live/` and `sources/*/live*/`;
- Claude filename conventions used by the incremental parser;
- Claude config and analytics parsers used by shared attachment;
- Codex `onSessionStart`, `onMessageWritten`, and `onSkippedRecord` official-token attribution plus `token-usage.ts`;
- Grok metadata/slug helpers and sidecar application used by its live watcher;
- the TypeScript query and app-service layers.

Every deletion phase must have a guard test proving Codex live official-token attribution and Grok live sidecars still work.

---

## Phase 0 — Eligibility, inventory, and deletion seams

### 1. Verify the RFC 008 handoff

Copy the readiness report identifier, package versions, target matrix, warm strategy, and token policy into this RFC's implementation PR. Do not rely on “latest main.”

### 2. Freeze KEEP and DELETE manifests

Produce machine-checkable manifests for:

- retained live-plane modules;
- TS bulk-only modules;
- engine-selection symbols and environment variables;
- native live-batch symbols;
- public exports being removed;
- managed-default `rebuildIndex()` and corrupt-cache recovery call sites;
- tests to rewrite versus tests to delete.

### 3. Split the Claude parser facade

`ClaudeCodeParserImpl` currently couples retained config/analytics parsing to the deletable project parser. Introduce a retained shared parser or instantiate config/analytics directly. Remove project parsing from the facade only after construction and integration tests prove shared parsing is independent.

### 4. Classify the session-completion hook

Apply RFC 008's token decision:

- delete the TS estimator and completion-only hook state after the Rust replacement ships; or
- delete it with an explicit “estimation dropped” behavior if RFC 008 chose that policy.

The retained live attribution path must not depend on `onSessionComplete`.

### Exit gate

- Readiness dependency verified.
- KEEP/DELETE manifests reviewed.
- Parser facade split is merged with no behavior change.
- Live guard tests pass under both selectable bulk engines.
- No engine/default/database behavior changed yet.

---

## Phase 1 — Deprecation and dormant bootstrap release

This phase ships while both engines and per-engine databases still work.

### Deprecate the selection surface

- `engine` SDK options, `SPAG_ENGINE`, `SPAG_NATIVE_INGEST`, playground engine settings, and `spag engine` print a removal notice.
- Native remains the default; explicit TS selection still works.
- Documentation identifies the final release that will support TS selection.
- `EngineUnavailableError` is shown by doctor and engine diagnostics but does not yet remove the explicit TS escape hatch.

### Land the single-DB bootstrap controller dormant

Implement and test the controller described below, but do not change the default path or invoke it in production. Dormant code must be callable from tests using an isolated cache directory.

### Prepare golden tests

- Add deterministic normalization for filesystem-derived timestamps and paths.
- Include `source_files` and fixed FTS queries.
- Generate candidate Rust goldens while the cross-engine harness still exists.
- Do not delete parity tests yet.

### Exit/release gate

- Deprecation release is published.
- Explicit TS rollback remains proven.
- Bootstrap crash/concurrency tests pass without touching user defaults.
- Candidate goldens match the RFC 008 readiness fixtures.

---

## Clean-rebuild database bootstrap

### Paths

- New default: `~/.spaghetti/cache/spaghetti.db`.
- Old caches: `spaghetti-rs.db` and `spaghetti-ts.db`. They remain the active per-engine paths through Phase 2. From Phase 3 onward the bootstrap/native path never adopts, moves, rewrites, or deletes them; an explicitly selected TS rollback may still update `spaghetti-ts.db` normally.
- On the managed-default path, a pre-split `spaghetti.db` is stale and must never be opened as the new cache without a successful bootstrap identity.
- An SDK caller's explicitly supplied `dbPath` bypasses this protocol completely.
- Playground uses the same protocol for its application-managed default inside its own cache directory. A genuinely user-supplied playground override bypasses it; the app's current internal act of passing its chosen default to the SDK does not.

### Durable files

- Coordinator: `spaghetti.bootstrap-control.db`, a metadata-only SQLite file used to hold a cross-process `BEGIN IMMEDIATE` lock. It is not a transcript cache and remains after cutover.
- State: `spaghetti.bootstrap.json`, written by flushing a temporary file and atomically replacing the prior marker.
- State shape:

  ```ts
  type BootstrapState =
    | {
        version: 1;
        state: 'building' | 'ready';
        buildId: string;
        schemaVersion: string;
        buildPath: string;
        quarantineBase?: string;
        at: string;
      }
    | {
        version: 1;
        state: 'settled';
        buildId: string;
        schemaVersion: string;
        at: string;
      };
  ```

- After ingest, the temporary DB stores the same `buildId` under the `bootstrap_build_id` key in `schema_meta`; its normal schema metadata must equal `schemaVersion`. Identity and expected schema version, not filename presence, determine whether a published target belongs to this bootstrap.
- Before any marker-derived file operation, resolve and verify `buildPath` and `quarantineBase` are generated, direct children of the expected cache directory with the expected basename pattern. Reject traversal, symlink/reparse-point escape, and malformed suffixes; never act on an unvalidated path.

### Authoritative-lock rule

Every managed-default caller briefly acquires coordination before making a bootstrap/open decision. An unlocked marker read may be used only to display status; it never authorizes opening or modifying the target.

1. open the coordinator and acquire or wait for `BEGIN IMMEDIATE`;
2. after acquiring it, discard every pre-lock observation;
3. re-read state, target identity, build path, and quarantine path while holding the transaction;
4. choose a transition from that fresh snapshot;
5. for a valid settled target, release coordination and open it normally; otherwise complete recovery/build/publish before release.

A waiter may display status from the marker while blocked, but it does not open or modify the target. Waiting is bounded and cancellable; timeout reports the coordinator path and never steals a live lock. SQLite releases the lock if the owner process crashes, so there is no PID-age or stale-lock deletion algorithm. A waiter that eventually acquires coordination re-reads the settled state and never resumes from cached `building` state.

No automatic transition replaces a healthy settled target. If the target disappears between releasing coordination and opening it, retry once through coordination instead of guessing or opening a newly appeared file.

### Build and publish protocol

1. While holding coordination, choose a unique same-directory `buildPath`, random UUID `buildId`, and the binary's expected `schemaVersion`, then write `state: building` before creating the DB. This ordering prevents an untracked temporary DB if the process crashes.
2. Run a full Rust ingest for every configured source into the new build path. No old database participates. After schema initialization and all source ingests finish, write `bootstrap_build_id = buildId` to `schema_meta`.
3. After ingest, run and verify `wal_checkpoint(TRUNCATE)` on a writable build connection, close every build connection, require no non-empty sidecars, then reopen read-only to run `quick_check` and verify the build identity and schema version.
4. Write `state: ready` while retaining `buildPath` and `buildId`.
5. If `spaghetti.db` exists without the same build identity, choose a unique `quarantineBase` and persist it in the ready marker **before** moving anything. Move the exact SQLite bundle (`-wal`, `-shm`, `-journal`, then main) to that base without overwriting any destination. The helper resumes idempotently from the marker after any partial move and fails closed on a source/destination collision.
6. Rename the validated build main file to `spaghetti.db` in the same directory and flush the directory entry where the platform supports it.
7. Re-open read-only, verify `quick_check`, `buildId`, and `schemaVersion`, then replace the marker with `state: settled` while still holding coordination.
8. Commit/close the coordinator transaction.

If quarantine or publish fails because another process still has a file open, retain `state: ready`, report the exact path, and retry later. Never fall back to deleting either bundle.

A rejected native ingest or structural database error never advances to `ready`. Non-fatal record/project/source errors follow RFC 008: surface them, keep the affected source marker incomplete, and publish only if the database itself validates, so the normal next warm run retries the affected input.

The controller returns the per-source ingest stats and the published build counts as that initialization's cold bulk pass. Continue through the normal warm check after publish—it should no-op when every source completed and may make one retry when a source marker is incomplete—but never launch a second unconditional cold build in the same initialization.

Old per-engine caches and legacy quarantine files remain during the rollback window.

### Crash recovery

- missing state: start a fresh build under coordination; any pre-existing target is handled as stale during publish.
- `building`: discard only the exact temporary SQLite bundle named by the marker after verifying it is a direct child of the expected cache directory; start a fresh build.
- `ready` + valid build temp: resume the exact quarantine/publish operation recorded by `quarantineBase`, if present.
- `ready` + target has matching `buildId`: publication completed; validate and settle.
- `ready` + neither valid target nor valid temp: start a fresh build.
- `settled` + matching build identity and expected schema version: open normally.
- `settled` + structurally healthy but incompatible schema version: clean-rebuild under coordination; never let downstream schema initialization wipe the managed target in place.
- `settled` + missing/invalid target: acquire coordination and rebuild cleanly with a new `buildId`.
- Any unknown state version or malformed marker fails closed with an actionable recovery message; it does not guess from filenames.

### Test matrix

- fresh directory;
- stale pre-split `spaghetti.db` with WAL/SHM/journal variants;
- old rs only, ts only, and both—assert the bootstrap leaves them byte-identical;
- crash during build;
- crash after ready marker;
- crash during each legacy bundle move;
- crash after publish before settle;
- corrupt build and failed `quick_check`;
- two-process race at every coordination/state boundary;
- waiter re-reads settled state after acquiring coordination;
- coordinator owner crash, bounded live-lock wait, cancellation, and timeout without lock stealing;
- state/target identity mismatch;
- settled target with an older/newer schema version;
- marker path traversal, unexpected suffix, and symlink/reparse-point escape;
- target deleted after settle;
- custom `dbPath` bypass;
- playground cache variant.

No test operates on a developer's real cache directory.

---

## Phase 2 — Native-required canary release

Keep the old default database paths for this phase so native-required behavior is isolated from database bootstrap behavior.

### Behavior

- Rust remains the preferred default, but omitted/default selection now requires the native addon instead of automatically falling back; all first-party entrypoints use this behavior.
- The explicit deprecated TS selection remains available solely as rollback.
- Automatic native-load fallback is removed: default/native selection throws `EngineUnavailableError`.
- `spag engine` reports native version/availability and marks TS as emergency/deprecated.
- CI runs native-first SDK, CLI, and playground integration suites on all supported hosts.

### Canary gate

- Publish the canary release.
- Exercise real Claude, Codex, Grok, and multi-source corpora.
- No open issue may involve stale warm results, missing rows, partial projects, silent errors, token double-counting, or advertised-platform load failure.
- Explicit TS rollback from the same release remains green.

If the gate fails, fix RFC 008 behavior and issue another canary. Do not enable the single DB at the same time as the fix.

---

## Phase 3 — Single-database cutover release

### Enable bootstrap

- For every service without a caller-owned `dbPath`, use `spaghetti.db` whenever native is selected, whether selection was omitted, persisted, environmental, or an explicit legacy `rs` value.
- Run the clean-rebuild bootstrap before opening the shared TypeScript data connection.
- Show progress that clearly says a one-time index rebuild is occurring.
- Continue to use old `spaghetti-ts.db` only when the explicit deprecated TS rollback is selected.
- The bootstrap/native path does not alter either old cache; only an explicitly selected TS rollback may write `spaghetti-ts.db`.

### Rebuild and corruption behavior

- On a healthy settled managed-default DB, `rebuildIndex()` keeps the file and `bootstrap_build_id`, invalidates every configured source marker, and performs the RFC 008 full-source rebuilds in place. It does not rerun the one-time publish protocol.
- On a missing, structurally invalid, or schema-incompatible managed-default DB, `rebuildIndex()` and automatic recovery close this process's data handles and enter the bootstrap recovery path under coordination. They do not raw-wipe a managed target behind the state marker.
- After controller validation, managed-default schema initialization is create/verify-only. Disable the legacy schema-mismatch table wipe for this path; caller-owned paths retain their separately documented behavior.
- Caller-owned `dbPath` continues to bypass bootstrap; its explicit rebuild behavior remains caller-scoped and is tested separately.
- Multi-source rebuild tests prove every configured source returns, the bootstrap identity survives a healthy rebuild, and failures remain retryable.

### Rollback properties

- Rolling back to the previous application release finds its old per-engine caches present and schema-compatible; bootstrap never altered them.
- Selecting TS in the cutover release uses its old cache and does not write the new native cache.
- Re-entering native mode validates or resumes bootstrap by marker/build identity.

### Release gate

- Publish the single-DB release while the TS escape hatch still exists.
- Complete the bootstrap matrix against packaged artifacts on Windows, macOS, GNU Linux, and musl Linux where applicable.
- Soak at least one full published release cycle.
- No automatic old-cache cleanup occurs during the soak.

Phase 4 cannot merge before this release gate passes.

---

## Phase 4 — Delete the TypeScript bulk engine

### Bulk-only SDK deletions

| Area                     | Delete                                                                                          |
| ------------------------ | ----------------------------------------------------------------------------------------------- |
| Worker pool              | `packages/sdk/src/workers/`, committed worker bundle, worker build entry                        |
| Claude bulk              | TS cold/warm/full-reparse/incremental driver code and project parser after the facade split     |
| Codex/Grok bulk          | TS reader classes and TS bulk lifecycle branches                                                |
| Parser exports           | deprecated project-parser shims/barrels and worker-pool exports                                 |
| Completion-only tokens   | TS estimator, `onSessionComplete` hook half, its state/API/types, according to RFC 008's policy |
| Bulk transaction helpers | `beginBulkIngest`/`endBulkIngest` after proving no live caller                                  |

### Native live-batch deletion

The retained live path is TypeScript. Delete the unused alternative route end-to-end:

- `crates/spaghetti-napi/src/orchestrate/live_ingest.rs`;
- crate-root re-export;
- `LiveRow`, `LiveBatchResult`, and `LiveRowId` types;
- native TS declarations;
- `IngestService.writeBatch` native branch, native fields/options/fallback flag/db-path capture, and row serializer;
- live-batch diff mode and native-routing tests.

Regenerate N-API bindings and verify no production symbol remains.

### Engine-selection removal

- Remove engine resolution/default-path helpers, selection semantics, lifecycle fields/branches, durable-store pins, registry threading, and static-plane threading. Retain only the deprecated SDK input/type aliases needed by the one-release detector below; they cannot affect execution.
- Remove all selection behavior. For this release only, legacy SDK `engine` input, persisted engine settings, `SPAG_ENGINE`, `SPAG_NATIVE_INGEST`, and playground `SPAGHETTI_ENGINE` are ignored in favor of native and produce a once-per-process compatibility warning when present.
- Replace `spag engine` with a non-mutating tombstone command that explains removal; remove the TUI engine badge/settings.
- Remove playground engine IPC, settings, renderer controls, loading labels, and utility-host branching.
- Replace engine doctor output with native addon version or the actionable unavailable reason.

### Tests

- Rewrite engine-neutral behavior tests against native ingest.
- Keep workflow, subagent-meta, project-collision, registry-construction, integration, multi-source, source-dimension, Grok, Codex-live, and live-attribution coverage.
- Delete only engine-selection assertions and worker internals with no retained behavior.
- Replace cross-engine diffs with committed deterministic native goldens.
- Keep warm mutation, error protocol, bootstrap, and benchmark gates from RFC 008/this RFC.
- Native-dependent tests skip with an explicit build prerequisite only where the addon truly is unavailable; CI jobs that advertise native support must build and run them.

### Exit gate

- `pnpm build && pnpm typecheck && pnpm test` passes from a fresh clone with documented native prerequisites.
- KEEP-list live tests pass.
- Searches for removed engine, worker, parser, estimator, and live-batch symbols return only historical documents and the documented compatibility shims.
- Published package contents contain no worker bundle or dead native live-batch export.

---

## Phase 5 — Final compatibility and cleanup release

### Compatibility cleanup

- Remove the one-release legacy engine detectors/warnings and the `spag engine` tombstone.
- Remove deprecated SDK input/export aliases retained solely for the cutover window.
- Update README, SDK/CLI docs, site API/commands, architecture diagrams, coverage claims, releasing docs, and changelog to the final single-engine model.
- Add supersession notes to RFCs whose non-goals promised permanent dual engines.

### Old-cache cleanup

Old per-engine caches and bootstrap quarantines are not silently deleted.

- Doctor reports their paths and approximate sizes.
- Provide an explicit cleanup command or exact manual commands.
- Cleanup requires a valid settled target and confirmation.
- Resolve and validate every deletion target inside the expected cache directory; never use a broad glob or home-directory recursive delete.
- State clearly that cleanup removes rebuildable cache data and is irreversible except by re-ingest.
- Never treat `spaghetti.bootstrap-control.db` or the settled state marker as an old cache; normal operation still uses them.

### Closure gate

- Compatibility release is published.
- Upgrade and rollback documentation reflects the final supported versions.
- Site and package docs match actual exports and commands.
- Optional cleanup is tested against isolated cache layouts.
- No old cache is required for normal operation.

---

## Golden-test contract

After the TS engine is gone:

- normalize fixture-root paths and path separators;
- replace filesystem-derived timestamps with stable sentinels rather than rounding;
- snapshot canonical tables, relevant derived tables, and `source_files`;
- snapshot fixed FTS queries including ordered message IDs and snippets;
- keep source-specific fixtures and mixed-source fixtures;
- regenerate only through an explicit `--write-golden` command;
- CI compares without rewriting.

Goldens complement, not replace, RFC 008's mutation/error tests.

---

## Rollback by phase

| Phase shipped | Rollback                                                                                         |
| ------------- | ------------------------------------------------------------------------------------------------ |
| 1             | Select either engine as before; bootstrap is dormant                                             |
| 2             | Explicitly select TS and use its unchanged cache                                                 |
| 3             | Explicitly select TS or install the prior release; old caches remain available and unmigrated    |
| 4             | Install the last Phase 3 release; it can still use old TS cache and the settled new native cache |
| 5             | Install the last Phase 3 release if TS emergency fallback is required                            |

The new database is always rebuildable. Rollback never requires reverse-migrating it into an old engine cache.

---

## Overall completion criteria

- RFC 008 readiness dependency remained satisfied through cutover.
- Native-required canary and single-DB cutover shipped as separate gated releases.
- Unified DB was built cleanly, not adopted from a donor.
- Old per-engine caches survived the rollback window without adoption, migration, or automatic deletion.
- TS bulk code, worker bundle, engine selection, and native live-batch route are gone.
- TS live writes and all live behavior remain covered.
- Deterministic native goldens, warm/error matrices, bootstrap tests, and benchmark gate form the final safety net.
- Missing native addon fails loudly with actionable text.
