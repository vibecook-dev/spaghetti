# RFC 008: Rust Bulk Ingest Production Readiness

**Status:** Draft v1
**Created:** 2026-08-07
**Author:** James Yong + Kimi
**Depends on:** [RFC 003 — Rust Ingest Core](./003-rust-ingest-core.md) · [RFC 004 — Rust Ingest Follow-ups](./004-rust-ingest-followups.md) · [RFC 006 — Normalized Message Model](./006-normalized-message-model.md)
**Blocks:** [RFC 009 — Retire the TypeScript Bulk Ingest Engine](./009-retire-typescript-bulk-ingest.md)
**Independent of:** [RFC 007 — Retire the Runtime Bridge](./007-retire-runtime-bridge.md)
**Phase records:** [Phase 0 — contract freeze and baseline](./008-phase-0-baseline.md) · [Phase 1 — warm reconciliation gate](./008-phase-1-gate.md)

---

## Summary

Make the Rust cold/warm ingest engine correct, observable, portable, and proven enough to become the sole bulk engine later.

This RFC deliberately keeps the TypeScript bulk engine, engine selection, per-engine database files, and the cross-engine diff harness. They are the safety net used to prove this work. Deleting them belongs to RFC 009 and cannot start until this RFC has shipped and completed its soak gate.

The work is divided into six independently reviewable phases:

1. Freeze the observable contract and establish evidence.
2. Make warm reconciliation converge, including upgrade repair.
3. Make project transactions and error reporting correct end-to-end.
4. Decide and implement Codex token estimation from fixtures rather than assumptions.
5. Establish performance and platform readiness.
6. Ship the result and complete a dual-engine soak.

---

## Why this is its own RFC

The Rust engine currently works well enough to compare with TypeScript, but “works in the common case” and “safe to make mandatory” are different standards. Retirement pressure previously caused correctness, platform support, migration, and deletion design to be solved simultaneously. That made every unresolved edge case a cutover blocker and encouraged premature state-machine design.

This RFC removes the deadline. The fallback remains available while the native engine earns a readiness artifact with explicit evidence. RFC 009 consumes that artifact; it does not reinterpret it.

---

## Scope

This RFC owns:

- warm-start detection and convergence for Claude Code, Codex, and Grok;
- complete source-data clearing and upgrade repair;
- fingerprint coverage and its documented detection limits;
- project transaction boundaries and terminal events;
- error identity, retry behavior, aggregation, and public wire types;
- Codex token-estimation parity or an explicit decision to drop it;
- warm performance measurement;
- native package/platform availability and actionable failure diagnostics;
- a dual-engine soak and a signed-off readiness report.

---

## Non-goals

1. Do not delete the TypeScript bulk engine, workers, parsers, or tests.
2. Do not remove engine settings, environment variables, or `spag engine`.
3. Do not consolidate or rename database files.
4. Do not move the TypeScript live writer into Rust.
5. Do not remove `live_ingest_batch`; RFC 009 decides its final fate.
6. Do not retire the runtime bridge or plugins; RFC 007 owns that.
7. Do not add a new transcript source.
8. Do not claim content-change detection stronger than the fingerprint data can prove.

Schema changes are not forbidden. If a correctness property requires durable state that existing columns cannot represent, this RFC must prefer an explicit, versioned schema change over overloading an unrelated bit.

---

## Readiness decisions

| ID  | Decision                            | Default                                                                                                                                 |
| --- | ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| P1  | Changed-source warm strategy        | Correct full-source clear-and-reingest first; consider per-project incremental only after measurement                                   |
| P2  | Error granularity                   | Bad record = skip and report; unreadable project input = abort the project; pre-identity failure = source error                         |
| P3  | Fingerprint guarantee               | Detect additions, deletions, and changes visible through recorded metadata; do not promise same-size/same-mtime detection               |
| P4  | Codex token estimates               | Preserve only if a fixture-backed model can avoid overwriting or double-counting official usage; otherwise explicitly drop              |
| P5  | Platform baseline                   | Publish musl targets, record the glibc floor, and make every addon-load failure visible and actionable                                  |
| P6  | Cutover evidence                    | At least one published dual-engine release must complete the Phase 5 soak before RFC 009 is eligible                                    |
| P7  | Failed project after a source clear | Keep successfully rebuilt projects, leave the failed project absent, report it, and retry; do not restore stale rows from the old cache |
| P8  | Fingerprint ownership               | Key and query fingerprints by `(source_id, path)`; never infer source ownership with raw string-prefix matching                         |

Changing a default requires editing this table and the affected exit gate.

---

## Correctness contracts

### Warm convergence

After a successful changed-source or forced-repair warm run:

- canonical and derived rows for that source equal the rows derivable from successfully read inputs;
- rows for other sources are unchanged;
- failed files/projects are not fingerprinted as successfully ingested;
- the next warm run retries failed inputs;
- an unchanged fast path is allowed only when fingerprints and the ingest-contract version both match.

### Fingerprint detection boundary

The current fingerprint schema records path, mtime, size, and optional byte position. Therefore the guaranteed detection set is:

- path addition or deletion;
- mtime change in either direction;
- size change;
- explicit ingest-contract version change.

A content rewrite that preserves both size and observable mtime is outside this RFC's guarantee. If that guarantee becomes required, add a content hash in a separate schema phase; do not continue saying “any change” while storing no hash.

### Project atomicity

A project has exactly one outcome:

- `ProjectComplete`: all project data commits, then its fingerprints may commit;
- `ProjectAbort`: all project data rolls back and every buffered fingerprint for that project is discarded.

Starting the next project is not a substitute for a terminal event.

Inputs that do not naturally belong to a project use a deterministic internal transaction unit and the same complete/abort/fingerprint rules. Claude global plans, for example, use the existing plans pseudo-project. An internal transaction key is not exposed as a real user project slug.

Because the Phase 1 baseline clears a changed source before rebuilding it, an aborted project has no current rows after that run. The run is visibly incomplete and retries; it does not silently resurrect a stale pre-clear snapshot. Preserving prior rows would require a shadow-source replacement protocol and is outside this RFC.

### Error delivery

Every surfaced error has a path and severity. Project identity is optional because discovery and pre-identity reads can fail before a slug exists. Errors are visible through Rust stats, generated N-API declarations, the handwritten SDK type, lifecycle owners, and CLI/TUI reporting. Any error that withholds a fingerprint leaves the source run incomplete and prevents success-marker publication, so a later warm run retries it.

---

## Phase 0 — Contract freeze and evidence baseline

### Fixtures

Create or confirm committed fixtures for:

- Claude small and medium corpora;
- Codex sessions with official `token_count`, no `token_count`, total-only counts, mixed coverage, empty/internal-only records, and live-growth tails;
- Grok session-level token/sidecar behavior;
- multi-source slug collisions;
- every input shape consumed by either bulk engine and expected to survive cutover.

The fixture README must map each behavior to a concrete file. “Covered by medium” is not sufficient.

### Baseline artifacts

1. Run the existing TS/Rust diff harness on all fixtures and record expected divergences.
2. Snapshot table dumps, source fingerprints, token rollups, and fixed FTS searches.
3. Record cold, unchanged-warm, and changed-warm timings on small, medium, and a real large corpus.
4. Add a per-source ingest-contract marker to existing metadata storage, for example a `source_materializations` row keyed by `(source_id, 'rust-ingest-contract')`. A global key is insufficient, and merely adding the representation does not mark any source repaired.
5. Define the public native error result before changing parser behavior:

   ```ts
   type NativeIngestError = {
     slug?: string;
     path: string;
     severity: 'record-skip' | 'project-fatal' | 'source';
     message: string;
   };

   type NativeIngestStats = {
     // existing counters
     errors: NativeIngestError[]; // first N
     errorCount: number; // uncapped total
     errorsTruncated: boolean;
   };
   ```

### Exit gate

- Fixtures and baseline results are committed.
- Known TS/Rust differences are enumerated rather than normalized away.
- The contract version and error wire shape are approved.
- No production behavior has changed.

---

## Phase 1 — Warm reconciliation and upgrade repair

### 1. Complete source clearing

`ClearSourceData` remains atomic under `BEGIN IMMEDIATE`.

- Source-scoped tables and projections clear by `source_id`.
- The seven artifact tables without `source_id`—`project_memories`, `workflows`, `tool_results`, `todos`, `tasks`, `plans`, and `file_history`—are fully cleared only when clearing `claude-code`, their sole writer today.
- Codex/Grok clears skip those seven tables.
- Children clear before parents.

The ownership assumption is tested and documented. A future source that writes one of these tables must first add ownership to the schema or revise this rule.

### 2. Make fingerprint ownership source-native

- Migrate `source_files` from `PRIMARY KEY(path)` to `PRIMARY KEY(source_id, path)` while preserving existing `source_id` values.
- Load, diff, upsert, and delete fingerprints with an explicit `source_id`; do not load every source and recover ownership with `starts_with(root)`.
- Normalize stored/discovered paths consistently for the host platform. Any containment validation uses path components and documented case rules, not a raw string prefix.
- Update the retained TS engine and live writer for the new key before shipping the schema, so RFC 008 rollback remains valid.

The migration test includes overlapping roots, roots whose names are string prefixes (`agent` versus `agent-old`), the same path under two source IDs, Windows separator/case cases, and rollback to the TS engine.

### 3. Force one upgrade repair

Historical builds can contain rows that no fingerprint diff reveals, including parent-less sidecars created after a rolled-back project. Therefore the new code must not wait for an ordinary file change.

- If a source's stored ingest-contract version is older than this RFC's version, force one complete source clear and re-ingest even when fingerprints match.
- Invalidate the source marker as part of the atomic source clear.
- Write the new version only after entity writes, derived rebuilds, and successful fingerprint publication finish with no omitted-fingerprint error.
- On error or crash, leave the marker absent or at the old version so the next warm run retries.
- The repair is source-specific; repairing Claude must not re-ingest Codex or Grok.

### 4. Cover every consumed input

Fingerprint discovery must include:

- plans;
- workflows and nested workflow transcripts;
- subagent `agent-*.meta.json` files;
- workflow `journal.jsonl` files;
- every existing session, task, todo, file-history, memory, and tool-result shape consumed by the parser.

If an input cannot be represented by the existing fingerprint model, the source must not take the unchanged fast path until a representation exists.

### 5. Handle absent roots as deletion

Do not return before looking at stored source state.

- If a Codex/Grok `sessions/` root is absent, run the idempotent source clear whenever any source-owned canonical or derived row exists.
- If Claude `projects/` is absent, clear project/session-derived rows but still run normal parsing for independent inputs such as global plans.
- The existence probe includes `projects`, `sessions`, `source_files`, and source-owned artifact/repair state; it is not limited to sessions and fingerprints.
- An absent source with no stored state remains a cheap no-op.

### Test matrix

- append, truncate, delete, and metadata-visible rewrite;
- session deletion, project deletion, and final root deletion;
- empty Claude project with no session file;
- global plans present while `projects/` is absent;
- add/change/delete for every sidecar category;
- multi-source preservation;
- overlapping/prefix-colliding source roots and duplicate paths across source IDs;
- seeded historical orphans with matching fingerprints followed by a normal unchanged warm start;
- injected crash before contract-version publication, followed by convergence on retry.

### Exit gate

- All matrix cases converge.
- A second warm run is a true no-op after successful repair.
- The contract marker is not published on partial success.
- Unchanged fast-path timing remains recorded; it need not yet meet the final performance threshold.

---

## Phase 2 — Transaction and error protocol

### Event model

Replace the overloaded worker error with explicit control events or an equivalent tagged shape:

```text
RecordSkip    { project_slug, path, message }
ProjectFatal  { project_slug, path, message }
SourceError   { path, message }
ProjectComplete { project_slug }
ProjectAbort    { project_slug }
```

Rules:

- `RecordSkip` records an error but does not roll back or poison the project.
- `ProjectFatal` rolls back the current project and causes subsequent data events for that project to be ignored.
- Every started project emits exactly one terminal event from a finally-style guard.
- `ProjectAbort` clears the bounded poison state without committing and discards the complete per-project fingerprint buffer.
- `SourceError` has no slug and never poisons a project. It invalidates the per-source success marker, disables the unchanged fast path, and remains retryable.
- Slug-switch tolerance in the writer cannot commit or clear a poisoned project.

### Fingerprint ordering

- Claude, Codex, and Grok use the same contract.
- Readers buffer fingerprints per project.
- Buffers emit only after `ProjectComplete`.
- Buffers are destroyed on `ProjectAbort`.
- Source-level discovery errors never gain a success fingerprint.
- Record-level errored paths are omitted from the emitted buffer so they retry.
- Standalone/global inputs use a deterministic internal transaction unit and follow the same buffering rules.

### Fallible discovery and readers

- Missing paths are absence, not an error.
- Permission, enumeration, stat, open, and mid-read failures are errors.
- Walkers do not use `flatten()`/`filter_map(ok)` where doing so hides an error.
- Pre-identity failures become `SourceError` through an explicit return/event path; no required fake slug is invented.

### Aggregation and public surface

- Keep the complete errored-path set internally even when displayed errors are capped.
- Return the first 100 errors, the uncapped total, and a truncation flag.
- Update the Rust `IngestError`, generated `index.d.ts`, handwritten `packages/sdk/src/native.ts`, and every adapter type together.
- Lifecycle owners capture `await native.ingest(...)` results instead of discarding them.
- Non-empty errors go to `errorSink` and a concise CLI/TUI warning naming affected files.

### Tests

- bad record between valid records;
- fatal first and second session in a two-session project;
- fatal final project;
- channel close immediately after fatal;
- pre-identity unreadable Codex file;
- disappearing file during discovery and during read;
- more than 100 errors, verifying total/truncation;
- later projects ingest fully after an abort;
- aborted project has zero rows and zero fingerprints.

Permission tests must be platform-aware: Unix may use mode bits; Windows requires an ACL/open-handle strategy or a deterministic filesystem fault seam. A Unix-only `chmod 0` test is not the sole cross-platform acceptance test.

### Exit gate

- Transaction/error matrix passes on Linux, macOS, and Windows.
- SDK consumers can access path and severity without type casts.
- A failed input is retried on the next warm run.
- No project can commit without exactly one successful terminal outcome.

---

## Phase 3 — Codex token estimation decision and implementation

Token estimation previously accumulated speculative fixes because the storage model was not characterized first. This phase deliberately separates the decision from the code.

### Phase 3A — Attribution model

Build fixture-backed traces for:

- per-turn `last_token_usage`;
- cumulative `total_token_usage` only;
- official first turn followed by an un-attributed turn;
- un-attributed first turn followed by a later official count;
- assistant-only and user-only tails;
- an already-estimated session that grows live;
- empty/internal-only sessions;
- multiple token-count events for one assistant.

For every trace, document which message/turn each official value covers. In particular, official input tokens stored on an assistant may cover preceding user/developer records; those records must not then receive duplicate estimates.

Choose one policy:

1. **Turn-aware port:** estimate only turns proven uncovered by official usage.
2. **Session-level fallback:** estimate only sessions with no official usage at all; mixed sessions remain partially unattributed.
3. **Drop estimation:** report only official usage.

The choice is recorded in this RFC before implementation. If neither existing message columns nor deterministic turn reconstruction can represent the chosen policy, an explicit provenance/pending table or column is allowed. `sessions.tokens_estimated` keeps its current meaning—true when the session contains estimates—and is never overloaded as a pending marker.

### Phase 3A exit gate

- Every fixture has an agreed expected token table and rollup.
- Pending/retry behavior is defined for new, official, estimated, and mixed sessions.
- The policy can distinguish “already processed but nothing estimable” from “not processed.”
- The decision identifies any required schema migration.

No estimator code lands before this gate.

### Phase 3B — Implement the chosen policy

If porting:

- use `tiktoken-rs` with the encoding selected by the attribution model;
- never overwrite official values;
- never estimate a user/developer row already covered by official turn input;
- preserve provenance through live growth and later official attribution;
- make pending state durable without reusing the provenance bit;
- port estimator unit tests and add the mixed/live-growth fixtures to the diff or explicit-divergence harness.

If dropping:

- remove estimation expectations from the readiness contract;
- document missing-token behavior in SDK/CLI/UI docs;
- make RFC 009 delete the TS estimator with no Rust replacement.

### Phase 3 exit gate

- The chosen fixture matrix passes.
- Empty/internal-only sessions do not cause warm loops.
- No official usage is overwritten or double-counted.
- The UI never labels estimates as API truth.
- The RFC 009 handoff records whether estimation was ported, narrowed, or dropped.

---

## Phase 4 — Performance and platform readiness

### Warm strategy

After Phases 1 through 3 are green:

1. Extend the benchmark with unchanged warm, one-day growth, deletion, and forced-repair scenarios.
2. Compare the correct full-source Rust warm path against the current TS incremental path on the same corpus and reference hardware.
3. Accept the full-source path when its median is no worse than `max(2 × TS median, 3 seconds)`; record the hardware, corpus size, run count, and raw samples.
4. Otherwise port per-project incremental deletion/reinsert and rerun the Phase 1/2 matrices unchanged.

Performance work may not weaken correctness tests or fingerprint/error semantics.

### Platform coverage

- Publish `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` in addition to existing targets.
- Build GNU artifacts on a pinned baseline and document the actual minimum glibc.
- Run a load-and-smoke test for every artifact, including Alpine containers for musl.
- Add `EngineUnavailableError` carrying platform, architecture, libc, addon version, and an install/upgrade hint.
- During this RFC, automatic fallback to the still-supported TS engine remains available, but it must emit the diagnostic and report the actual active engine. RFC 009 owns removal of automatic fallback.

### Exit gate

- Warm strategy decision and benchmark results are committed to this RFC.
- All advertised artifacts load on their target smoke environment.
- Supported-platform documentation matches published packages.
- Missing-addon behavior is loud and actionable.

---

## Phase 5 — Dual-engine soak and readiness report

Ship all prior phases while TypeScript remains selectable.

### Soak requirements

- At least one published minor release uses the new Rust behavior while retaining the TS fallback.
- Run both engines on small, medium, and real large corpora after every relevant fix.
- Keep the deterministic cross-engine harness and mutation/error matrices in CI.
- Resolve or explicitly accept every data divergence.
- No open correctness issue may involve data loss, stale warm results, project partial commits, silent parser failure, or unsupported advertised platforms.

### Readiness report

Commit a dated report containing:

- shipped package versions and native target matrix;
- fixture/golden identifiers;
- warm benchmark results and selected strategy;
- token-estimation decision;
- error statistics contract;
- known limitations, including metadata-only fingerprint detection;
- rollback instructions;
- explicit sign-off that RFC 009 may begin Phase 0.

### Completion gate

RFC 008 is complete only when the report is committed after the soak release. Passing tests on an unreleased branch is not sufficient.

---

## Rollback

- Every phase ships while TypeScript remains available.
- A Rust regression selects the TS engine and preserves the per-engine caches.
- Contract-version publication is success-last, so failed upgrade repair automatically retries.
- Any schema addition in Phase 3 must be backward-tolerant for the retained TS engine or land behind a versioned compatibility check.

---

## Handoff to RFC 009

RFC 009 may start only with all of the following:

- Phase 5 readiness report committed;
- no unresolved correctness-class divergence;
- token-estimation policy settled;
- supported-platform list published;
- `EngineUnavailableError` shipped;
- warm strategy measured and recorded;
- rollback to the TS engine proven on the soak release.

RFC 009 owns cutover mechanics. It may not weaken or waive these gates.
