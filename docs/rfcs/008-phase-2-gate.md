# RFC 008 Phase 2 — Transaction and Error Protocol Exit Gate

**Companion to:** [RFC 008](./008-rust-ingest-production-readiness.md) · [Phase 0 baseline](./008-phase-0-baseline.md) · [Phase 1 gate](./008-phase-1-gate.md)
**Captured:** 2026-08-09 · **Commits:** `9d12960`, `6e53667`, `6847b8a`, `0aa9282`, `0eb042f`
**Status:** Phase 2 exit gate met.

Phase 1 left two defects on purpose, because fixing them piecemeal would have
pre-empted this phase's design: per-record failures had no path into
`IngestStats.errors`, and one bad line cost the whole project. Both are closed
here, along with the swallowed-error problem that turned out to run much wider
than the error type.

---

## 1. One overloaded error became four events

`WorkerError` meant two incompatible things — "skip this record" and "this
project is unusable" — and the writer could only react one way: roll the whole
project back. Both emission sites meant the first. The parser's own comment
said *"swallow the bad line"*.

| Event | Transaction effect | Carries a slug |
| --- | --- | --- |
| `RecordSkip` | none — the project still commits | yes |
| `ProjectFatal` | rolls back, poisons the slug until its terminal event | yes |
| `ProjectAbort` | terminal counterpart to `ProjectComplete`; discards without committing | yes |
| `SourceError` | none, poisons nothing | **no** |

`SourceError` carries no slug because it happens before any project identity
exists. Inventing one is forbidden: a fake slug becomes a real row.

**The poison set is not defensive programming.** Parsers fill a local buffer
that is drained afterwards, so a project's data events can still be in flight
when its fatal lands. Without the poison they open a fresh transaction and
commit under whatever project comes next — the rolled-back rows reappearing
under a healthy neighbour. `late_data_for_a_poisoned_project_does_not_commit_under_its_successor`
is that case.

Slug-switch tolerance now rolls a poisoned predecessor back rather than
committing it. That tolerance exists to pick a sane boundary in interleaved
streams, not to override a rollback that already happened.

Every started project emits exactly one terminal event, from a single guard in
`parse_project`. No early return can leave the writer holding an open
transaction for a project nobody finished.

---

## 2. Fingerprints are a claim, and had been unconditional

A fingerprint is what lets the next warm start skip a file. Writing one for an
input that failed is therefore worse than writing none: the failure is recorded
as a success and never retried.

They are now buffered and flushed once every project has an outcome, keeping
only paths whose project reached `ProjectComplete` and which did not themselves
fail at any severity. Global inputs carry no slug and have no project to wait
for, so they need only the second rule, and ride the same single transaction.

**Deviation from the RFC, deliberate.** The RFC describes *readers* holding
these buffers. They are held in the writer instead, which keeps one choke point
for all three sources and preserves the pre-parse stat capture that guards
against concurrent appends — the orchestrator still stats before reading, and
only the write is deferred. The enforced contract is identical and the reasoning
is recorded at `Writer::flush_fingerprints`.

---

## 3. Discovery had been reading failure as emptiness

This was the widest problem, and it was not in the error type at all.
Enumeration used `Err(_) => return Ok(())` and `.flatten()` / `filter_map(ok)`
throughout, so a directory that existed but could not be read was
indistinguishable from one that was not there. The scan reported zero files,
warm start concluded nothing changed, and the index stayed stale with no sign
anything had gone wrong.

Absence and failure are now separate:

- **Absence is silent.** A machine that never ran an agent has no sessions
  directory, and that is nothing to report.
- **Everything else is an error** — permission denied, I/O failure, a path that
  is not a directory, a walkdir descent that failed.

Covers Claude discovery (nine enumeration sites plus the file-history walker),
and the Codex and Grok session walkers. All become `SourceError`, which
withholds the success marker and defeats the warm fast path in all three
sources: a directory we could not enumerate may hold changes we cannot see, so
"no differences found" is not "no differences".

Two more swallows surfaced while writing the matrix, both the same shape:

- `merge_with_discovered_entries` returned an empty vec when the project
  directory could not be enumerated. That listing is what tells us which
  sessions exist, so the merged index was built from `sessions-index.json`
  alone — complete-looking, missing every session on disk. Now `ProjectFatal`.
- Codex's `peek` swallowed read failures with `let _`, dropping an unreadable
  rollout before its identity was known. Now `SourceError`.
  Readable-but-unattributable stays silent, since a truncated rollout with no
  cwd is normal.

---

## 4. Reporting

`IngestStats` matches the shape frozen in Phase 0: `slug?`, `path`, `severity`,
plus `errorCount` and `errorsTruncated`.

Three things are tracked separately because they answer different questions:

| | Capped? | Why |
| --- | --- | --- |
| `errors` | 100 | Nobody scrolls 40,000 parse failures |
| `errorCount` | no | "3 failed" and "30,000 failed" are different situations |
| errored-path set | no | It decides which fingerprints are withheld — capping it would silently mark the 101st failure as ingested |

All three lifecycle owners awaited `native.ingest(...)` and discarded the
result. A partial ingest resolves successfully, so that return value was the
only place the failures existed. Each now routes them to the error sink; the
Claude owner had none, so `AgentDataServiceOptions` gained one.

The summary names the affected files, separates the severities, reports
`errorCount` rather than `errors.length`, and says the affected inputs will be
retried — which is true rather than reassuring, since §2 withholds their
fingerprints precisely so the next run re-reads them.

---

## 5. Cross-platform strategy

The gate requires the matrix to pass on Linux, macOS, and Windows, and rules
out a Unix-only `chmod 0` as the sole acceptance test.

**Where the filesystem can provoke it portably, it does.** A file standing
where a directory belongs makes `read_dir` fail with something that is not
`NotFound` on all three platforms — no mode bits, no ACLs, no privileged
runner.

**Where it cannot, a deterministic fault seam does.** A project directory that
exists but cannot be enumerated needs mode bits on Unix and ACLs on Windows;
the RFC permits a fault seam for exactly this. `project_parser::fault` compiles
to a `None` the optimiser deletes outside `cfg(test)`, is keyed by absolute
path so concurrently running tests cannot collide, and is always disarmed
through a scope guard.

---

## 6. Matrix coverage

| RFC case | Test |
| --- | --- |
| bad record between valid records | `phase_2_gate::a_bad_record_between_valid_records_keeps_the_project` |
| fatal project, later projects still ingest | `a_fatal_project_rolls_back_and_later_projects_still_ingest` |
| fatal final / only project | `a_fatal_on_the_only_project_commits_nothing_and_still_returns` |
| aborted project has zero rows and zero fingerprints | `an_aborted_project_leaves_no_rows_and_no_fingerprints` |
| late data after a fatal | `late_data_for_a_poisoned_project_does_not_commit_under_its_successor` |
| pre-identity unreadable Codex file | `codex::peek` → `SourceError`, §3 |
| file disappearing during discovery | `a_file_that_vanishes_after_discovery_is_absence_not_an_error` |
| more than 100 errors, total and truncation | `more_than_a_hundred_errors_report_total_and_truncation` |
| exactly one terminal event | `a_fatal_project_emits_one_terminal_event_and_never_completes`, `a_healthy_project_emits_exactly_one_terminal_event` |
| unreadable directory is not an empty scan | `phase_2_gate::an_unreadable_directory_is_an_error_not_an_empty_scan` |
| a failed input retries | `phase_2_gate::a_failed_input_is_retried_on_the_next_warm_run`, `a_fatal_project_retries_and_recovers` |

`a_clean_run_still_fingerprints_everything` guards the opposite direction:
withholding everything would look like success while making every warm start
cold.

---

## 7. Exit gate

| Gate | Status |
| --- | --- |
| Transaction/error matrix passes on Linux, macOS, Windows | ✅ §5, §6 — CI runs all three |
| SDK consumers reach path and severity without type casts | ✅ `NativeIngestError` and its severity union exported from the SDK index |
| A failed input is retried on the next warm run | ✅ §2, proven end to end — corrupt, withhold, defeat the fast path, repair, ingest clean |
| No project commits without exactly one successful terminal outcome | ✅ §1, single guard plus the poison set |

220 Rust tests, 466 SDK, 113 package. Parity unchanged: zero diffs on `small`,
`medium`, and `small-grok`; `small-codex` at its six known token-estimation
diffs, which decision **P4** owns in Phase 3.

**Phase 3 may begin.** It decides the Codex token-estimation question that the
Phase 0 baseline enumerated and every phase since has carried forward.
