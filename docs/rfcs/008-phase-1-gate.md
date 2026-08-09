# RFC 008 Phase 1 — Warm Reconciliation Exit Gate

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [Phase 0 baseline](./008-phase-0-baseline.md)
**Captured:** 2026-08-08 · **Commits:** `51c400a`, `09aca56`, `29851ab`, `9b22524`, `ebf907c`
**Status:** Phase 1 exit gate met.

Phase 1 makes a warm start produce the same index a cold start would. The five
implementation items landed first; writing the gate's test matrix afterward is
what found the defects below, all of which are fixed here.

---

## 1. What the matrix found

The matrix ingests a tree warm, rebuilds the **same tree cold into a fresh
database**, and requires the two to be identical — then requires the next warm
run to be a true no-op. Six of the first nine cases failed. Three distinct
defects, none of which the pre-existing tests could see, because every one of
them asserted against a hand-written expectation rather than against cold.

### 1.1 The warm rebuild never deleted

Reaching the parse means either a cold run or a warm run that found changes,
and both re-read everything. But the Claude path only sent `ClearSourceFiles`,
and only *after* the parse, so fingerprints converged while entity rows were
upsert-only. Anything that shrank or disappeared survived:

| Input change | Warm kept |
| --- | --- |
| Session truncated 2 lines → 1 | the dropped message |
| Session rewritten shorter | the stale message |
| Session file deleted | the session row and all its messages |
| Sidecars deleted | their todos, subagents, and plans |
| `projects/` removed | every project, forever |

Codex and Grok already sent `ClearSourceData` before reading. Claude was the
outlier; it now does the same, which subsumes the narrower absent-`projects/`
clear added in 1.5.

This is decision **P1** — *"correct full-source clear-and-reingest first"* —
finally holding for all three sources. It also explains Phase 0's timing:
changed-warm already cost a cold start, so the work was being redone in full.
Only the delete was missing.

### 1.2 Empty project directories were invisible to warm

Fingerprints track files, so creating or removing a project directory that
contains none is a change no file changed. A cold run indexes it anyway,
because the slug list comes from a directory scan — so warm and cold disagreed
on exactly the empty projects. `warm_has_no_changes` now also compares the
scanned slug set against the indexed one for the source.

### 1.3 One bad line dropped a project *and* marked it clean

The most serious of the three, and the one the gate's own negative case
exposed. A malformed record emits `WorkerError`, and the writer rolls the
project's transaction back on one. But `IngestStats.errors` collects only what
a parser **returns** — project-level failures — so per-record errors never
reached it.

On a one-project fixture, appending a single unparseable line produced:

```
errors:            []
projects:          []      sessions: []      messages: []
contract_current:  true
```

Every good record in that project was discarded, nothing reported it, and the
contract marker was published over the loss. Because the marker is what defeats
the warm fast path, that made the repair unreachable.

The writer now counts rollbacks and publication requires **both** failure
channels clean. Failing to publish costs one extra re-ingest; publishing
wrongly costs the repair, so this errs in the safe direction.

---

## 2. Deliberately left to Phase 2

Two defects surfaced by 1.3 are **not** fixed here, because Phase 2 owns the
transaction and error protocol and fixing them piecemeal would pre-empt its
design:

1. **Per-record failures have no path into `IngestStats.errors`.** They travel
   as events and are now counted, but not surfaced.
2. **One bad line still costs the whole project**, not the record.

The `record-skip` / `project-fatal` / `source` severities frozen in Phase 0
exist precisely for this. Phase 1's obligation was narrower: stop the run from
claiming success.

---

## 3. Final root deletion does not clear

The one matrix case that deliberately diverges from "converge to cold". An
absent agent root is refused before any source dispatch, because a configured
path going missing is far more often a misconfiguration — wrong
`CLAUDE_CONFIG_DIR`, unmounted volume — than an intentional wipe, and clearing
on it would turn a typo into silent mass deletion.

Convergence here means the two modes agree and neither mutates: warm and cold
fail with the same error and leave the index untouched. A root that still
exists with its contents gone is the case that clears, and is covered.

---

## 4. Matrix coverage

All thirteen cases live in `crates/spaghetti-napi/src/orchestrate/ingest.rs`,
module `phase_1_gate`.

| RFC case | Test |
| --- | --- |
| append | `append_converges` |
| truncate | `truncate_converges` |
| metadata-visible rewrite | `rewrite_converges` |
| session deletion | `session_deletion_converges` |
| project deletion | `project_deletion_converges` |
| final root deletion | `root_deletion_fails_loudly_and_identically_in_both_modes` |
| empty project, no session file | `empty_project_with_no_session_file_converges` |
| global plans while `projects/` absent | `plans_without_projects_converges` |
| add/change/delete per sidecar category | `sidecar_add_change_delete_converges` |
| multi-source preservation | `multi_source_rows_survive_a_claude_repair` |
| prefix-colliding roots, duplicate paths across source IDs | `prefix_colliding_source_roots_and_duplicate_paths_converge` |
| seeded orphans, then unchanged warm start | `seeded_orphans_survive_until_the_contract_bumps` |
| crash before contract publication | `a_crash_before_contract_publication_converges_on_retry` |
| *(gate item 3)* marker withheld on partial success | `a_partial_success_does_not_publish_the_contract_marker` |

---

## 5. Timings

Same hardware and corpus as [Phase 0](./008-phase-0-baseline.md) — large corpus,
9 projects / 1,404 sessions.

| Scenario | Phase 0 | Now |
| --- | --- | --- |
| Unchanged warm (fast path) | 60 ms | **57–63 ms** |
| Changed warm | 2,500 ms | **2,610 ms** |
| Cold, for reference | 2,531 ms | **2,669 ms** |

Cold and changed-warm moved together by ~5%, so the clear is a small constant
plus run-to-run variance, not a change in shape. Changed-warm still costs a
cold start, and still lands under the 3-second absolute floor of
`max(2 × TS median, 3 s)` — Phase 4 may pass on that floor without per-project
incremental work, which remains the open question P1 defers.

The fast path now runs one extra directory scan and one indexed `SELECT` (§1.2);
the measurement above is why that is recorded as noise rather than assumed to be.

---

## 6. Cross-engine parity

Unchanged by this phase. Re-verified after every commit:

| Fixture | Source | Diffs |
| --- | --- | --- |
| `small` | Claude | 0 |
| `medium` | Claude | 0 |
| `small-grok` | Grok | 0 |
| `small-codex` | Codex | 6 — the known token-estimation divergence, decision P4 |

202 Rust tests, 457 SDK tests.

---

## 7. Exit gate

| Gate | Status |
| --- | --- |
| All matrix cases converge | ✅ 13 cases, §4 — three defects found and fixed |
| A second warm run is a true no-op after successful repair | ✅ asserted by `assert_converges` on every case |
| The contract marker is not published on partial success | ✅ §1.3, both failure channels now gate it |
| Unchanged fast-path timing recorded | ✅ §5 |

**Phase 2 may begin**, carrying the two deferred items in §2 as its first
concrete work: per-record errors need a channel into `IngestStats.errors`, and
the rollback boundary needs to be the record rather than the project.
