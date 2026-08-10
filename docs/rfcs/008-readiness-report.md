# RFC 008 — Rust Ingest Readiness Report

**Status:** ⚠️ **Draft — not signed off.** Every technical gate below is met, but the
completion gate requires a published soak release, which has not happened yet.
See §9.

**Dated:** 2026-08-09 · **Branch commit:** `d2a3713` · **Current version:** `0.5.23` (unreleased)
**Phases:** [0](./008-phase-0-baseline.md) · [1](./008-phase-1-gate.md) · [2](./008-phase-2-gate.md) · [3](./008-phase-3-gate.md) · [4](./008-phase-4-gate.md)

---

## 1. Shipped package versions and native target matrix

Versions are lockstep across `@vibecook/spaghetti-sdk`,
`@vibecook/spaghetti-sdk-native`, and the CLI. **The soak release has not been
cut**, so the version below is the current unreleased one; §9 is what remains.

Eight targets, each `require()`-loaded on a matching host before publish —
verified green on 2026-08-09:

| Platform | Arch        | libc         | Load-tested on                        |
| -------- | ----------- | ------------ | ------------------------------------- |
| macOS    | arm64 / x64 | —            | `macos-latest` (x64 under Rosetta 2)  |
| Linux    | x64 / arm64 | glibc ≥ 2.35 | `ubuntu-latest`, `ubuntu-24.04-arm`   |
| Linux    | x64 / arm64 | musl         | `node:24-alpine` on each architecture |
| Windows  | x64 / arm64 | —            | `windows-latest`, `windows-11-arm`    |

Full table: [`SUPPORTED-PLATFORMS.md`](../SUPPORTED-PLATFORMS.md).

---

## 2. Fixtures and goldens

| Fixture       | Source | Generator                     | Cross-engine |
| ------------- | ------ | ----------------------------- | ------------ |
| `small`       | Claude | `generate-ingest-fixture.mjs` | 0 diffs      |
| `medium`      | Claude | `generate-medium-fixture.mjs` | 0 diffs      |
| `small-grok`  | Grok   | `generate-grok-fixture.mjs`   | 0 diffs      |
| `small-codex` | Codex  | `generate-codex-fixture.mjs`  | 0 diffs      |

All four run in CI. Baselines under [`008-baseline/`](./008-baseline/); row
contents are hashed rather than dumped.

**Two fixture defects were found by later phases, both of the same kind — a
fixture that passed while testing nothing:**

- The Codex fixtures emitted turns as `event_msg`, which the extractor skips as
  a UI projection. Six sessions produced five messages and not one turn, so the
  token-attribution fixtures exercised no attribution (Phase 3A).
- The `small` fixture's subagent was never referenced by a `tool_result` in its
  parent session, so `unlinked` was correct in both engines and the Rust
  writer's hardcoded `unlinked` went unnoticed (Phase 5).

Both are fixed and both now carry the case they were meant to. The pattern is
worth remembering: **zero rows and correct rows both look like a passing test.**

---

## 3. Warm benchmark results and selected strategy

**Full-source clear-and-reingest. Per-project incremental was not implemented.**

Large corpus (1,404 sessions / 44 MB), both engines back to back:

| Scenario      | Rust   | TS     | Threshold | Margin     |
| ------------- | ------ | ------ | --------- | ---------- |
| Unchanged     | 60 ms  | 427 ms | 3 s       | 50× under  |
| Growth        | 2.61 s | 6.65 s | 13.3 s    | 5.1× under |
| Deletion      | 2.59 s | 6.63 s | 13.3 s    | 5.1× under |
| Forced repair | 2.57 s | n/a    | —         | —          |

Rust's full-source rebuild is **2.5× faster than TS's incremental path**. Raw
samples and hardware: [`008-phase-4a-warm-strategy.md`](./008-phase-4a-warm-strategy.md).

---

## 4. Token-estimation decision

**Policy 2 — session-level fallback, narrowed to completed turns.** Estimate
only when a session has no official usage _and_ at least one assistant reply.
No schema migration. Full rationale and per-fixture traces:
[`008-phase-3a-attribution.md`](./008-phase-3a-attribution.md).

RFC 009 note: the completed-turn guard lives in `ingest-hooks.ts` and
`reader.rs`, **not** in the estimator files, so deleting the TS estimator
wholesale would drop it.

---

## 5. Error statistics contract

`IngestStats` carries the shape frozen in Phase 0:

```ts
{ errors: NativeIngestError[]; errorCount: number; errorsTruncated: boolean }
{ slug?: string; path: string; severity: 'record-skip' | 'project-fatal' | 'source'; message: string }
```

`errors` caps at 100; `errorCount` does not. The internal errored-path set is
also uncapped, because it decides which fingerprints are withheld — capping it
would mark the 101st failure as successfully ingested.

Consumers reach `path` and `severity` without casts; the types are exported
from the SDK index. All three lifecycle owners route non-empty reports to the
error sink.

---

## 6. Data divergence status

Fixtures: **zero diffs on all four.** Real corpus (276 MB, 23 projects, 50,697
messages): **421 divergences**, down from 657 at the start of Phase 5.
Messages, todos, tasks, workflows, project memories, and FTS are clean.

**The real corpus is live**, so counts move between runs — it is a working
`~/.claude` that grows while the audit runs. Treat the numbers below as a
snapshot and the _shapes_ as the finding; only the fixtures are reproducible
enough to gate CI.

### Accepted

| Divergence                            | Count | Why accepted                                                                                                                                                                                                                                                                             |
| ------------------------------------- | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `subagents.agent_type`                | 412   | **Rust is correct and TS is wrong.** TS records the literal `"task"`; Rust reads the `agent-*.meta.json` sidecar and records the real type (`workflow-subagent`, `general-purpose`, `Explore`, `Plan`, …). Backporting sidecar reading to TS would be work on an engine RFC 009 deletes. |
| `sessions.created_at` / `modified_at` | 4     | 1 ms rounding: TS rounds the file mtime up, Rust truncates. Benign; no data is lost or misordered at that resolution.                                                                                                                                                                    |

### Resolved during Phase 5

| Divergence                                | Count | Resolution                                                                                                                                                                                 |
| ----------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `subagents.spawn_tool_id` / `link_method` | 226   | Rust never implemented linkage — the columns were filled with `NULL` / `"unlinked"` literals. Ported from TS. **This was data loss**, since RFC 009 deletes the engine that had the links. |

### Open — characterised, not yet resolved

| Table / field                    | Count | Note                                                                                                                                                                                                            |
| -------------------------------- | ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `projects.sessions_index`        | 1     | Was 2. Both engines appended discovered-but-unindexed sessions in directory order and neither sorted, which agreed on NTFS and disagreed on ext4/APFS; both now sort. One row still differs and is unexplained. |
| `plans.title`                    | 2     | Title extraction differs on 2 plans.                                                                                                                                                                            |
| `tool_results` (4 fields, 1 row) | 4     | All four fields differ on a single row, which reads as a row-ordering or off-by-one alignment difference rather than four independent bugs.                                                                     |
| `file_history.data`              | 1     | Snapshot blob differs on 1 row.                                                                                                                                                                                 |

**These are the last blocker to sign-off besides the release itself.** Ten
rows across four tables, none in the message or session bodies. Each needs to
be resolved or explicitly accepted before §9 can be signed.

---

## 7. Known limitations

- **Metadata-only fingerprint detection.** Warm start compares mtime and size,
  not content. A file rewritten to the same size within the same mtime tick is
  not detected. The forced-repair path (a contract-version bump) is the escape
  hatch.
- **Fingerprint coverage differs by engine, deliberately.** Rust fingerprints
  38 files on the `small` fixture where TS fingerprints 16 — subagents, tool
  results, memory, todos, tasks, file history, plans, and workflow journals.
  Rust re-ingests where TS would wrongly stay warm; the asymmetry is in the
  safe direction. `source_files` is excluded from the diff harness, so this
  never shows as a diff.
- **Estimated tokens are attributed differently from official ones.** Official
  counts put the whole prompt's input on the assistant; estimates put input on
  user rows and output on assistant rows. The two never occur in the same
  session, which is the only reason this is tolerable.
- **The real-corpus diff is a manual audit, not a CI gate**, and cannot become
  one while the accepted `agent_type` divergence stands.

---

## 8. Rollback

The TypeScript engine remains selectable throughout.

- `SPAG_ENGINE=ts`, or `engine: 'ts'` in `createSpaghetti`, or the persisted
  `~/.spaghetti/config.json` setting.
- Per-engine database files (`spaghetti-rs.db` / `spaghetti-ts.db`) mean
  switching engines does not require a rebuild and cannot corrupt the other
  engine's cache.
- Contract-version publication is success-last, so a failed upgrade repair
  retries automatically rather than latching.
- No schema migration was needed for Phase 3, so nothing must be undone.
- `resolveActiveEngine()` reports the engine that actually ran; a fallback is
  reported to the error sink via `EngineUnavailableError`.

---

## 9. Sign-off

**Not given.** The completion gate is explicit: _"RFC 008 is complete only when
the report is committed after the soak release. Passing tests on an unreleased
branch is not sufficient."_

Remaining before sign-off:

1. **Cut and publish a minor release** carrying the Rust behaviour with the TS
   fallback retained — the soak requirement of "at least one published minor
   release".
2. **Resolve or explicitly accept the ten open divergences** in §6.
3. **Prove rollback on that release**, not on a branch.
4. Re-date this report and sign §9.

Until then RFC 009 may **not** begin Phase 0. Every other handoff condition is
met: the token-estimation policy is settled, the supported-platform list is
published, `EngineUnavailableError` has shipped, the warm strategy is measured
and recorded, and no unresolved divergence involves data loss, stale warm
results, partial project commits, or silent parser failure.
