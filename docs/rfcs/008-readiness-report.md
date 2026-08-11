# RFC 008 — Rust Ingest Readiness Report

**Status:** ⚠️ **Draft — not signed off.** Divergence, rollback, and
data-loss gates are all met on the published `0.6.2`, and the data loss is
demonstrably fixed for users (§8). One gate remains: a **stock `npm install`
on npm 12 still does not work**, and the remedies `0.6.2` printed were
themselves wrong. See §9 — what is left is a decision, not an investigation.

**Cross-platform verification:** [`008-handoff-mac.md`](./008-handoff-mac.md) — executed on macOS 2026-08-10 (§8 of that doc). Fixes in [#115](https://github.com/vibecook-dev/spaghetti/pull/115) and [#116](https://github.com/vibecook-dev/spaghetti/pull/116). As predicted, macOS found a *different* divergence set, not a subset: both open Windows items vanished and three new classes appeared, one of them silent data loss.

**Dated:** 2026-08-11 · **Verified on:** Windows 11 / NTFS and macOS 15 / APFS · **Current version:** `0.6.2` (published)
**Phases:** [0](./008-phase-0-baseline.md) · [1](./008-phase-1-gate.md) · [2](./008-phase-2-gate.md) · [3](./008-phase-3-gate.md) · [4](./008-phase-4-gate.md)

---

## 1. Shipped package versions and native target matrix

Versions are lockstep across `@vibecook/spaghetti-sdk`,
`@vibecook/spaghetti-sdk-native`, and the CLI.

**The soak release is cut.** `0.6.0` shipped the Rust behaviour with the TS
fallback retained, `0.6.1` added the CRLF plan-title fix, and `0.6.2`
(2026-08-11) carries the macOS parity work and the silent-parse-failure fix.
All three packages are published at `0.6.2` and `latest` points at it:

| Package                          | Published | Notes                                                          |
| -------------------------------- | --------- | -------------------------------------------------------------- |
| `@vibecook/spaghetti`            | `0.6.2`   | CLI                                                            |
| `@vibecook/spaghetti-sdk`        | `0.6.2`   |                                                                |
| `@vibecook/spaghetti-sdk-native` | `0.6.2`   | ships all 8 platform binaries bundled; `next` still on `0.6.0-rc.0` |

The native package carries every target in the one tarball (`files: ["*.node"]`,
no `optionalDependencies`) — verified by unpacking the published artifact, which
also confirms the `crates/spaghetti-napi/npm/*` platform packages are vestigial
(see the handoff's trap list).

**Caveat that blocks sign-off:** the published CLI still does not work on a
stock npm 12 install, on any of these versions. See §9.

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

Fixtures: **zero diffs on all four**, on both Windows and macOS.

Real corpus, two platforms:

| Platform          | Corpus                            | Divergences                                    |
| ----------------- | --------------------------------- | ---------------------------------------------- |
| Windows 11 / NTFS | 276 MB, 23 projects, 50,697 msgs  | 421 (from 657 at the start of Phase 5)         |
| macOS 15 / APFS   | 113 projects, 42,783 msgs         | 360 → **223**, all of them accepted `agent_type` |

**Every non-accepted divergence is closed.** On Windows, messages were clean
and the open items sat in metadata; on macOS `messages` was *not* clean until
#115 — the platform that found the data loss was the second one, which is the
whole argument for cross-platform verification.

**The real corpus is live**, so counts move between runs — it is a working
`~/.claude` that grows while the audit runs. Treat the numbers below as a
snapshot and the _shapes_ as the finding; only the fixtures are reproducible
enough to gate CI.

### Accepted

| Divergence             | Count | Why accepted                                                                                                                                                                                                                                                                             |
| ---------------------- | ----- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `subagents.agent_type` | 412   | **Rust is correct and TS is wrong.** TS records the literal `"task"`; Rust reads the `agent-*.meta.json` sidecar and records the real type (`workflow-subagent`, `general-purpose`, `Explore`, `Plan`, …). Backporting sidecar reading to TS would be work on an engine RFC 009 deletes. |

> **Retracted 2026-08-10:** `sessions.created_at` / `modified_at` was accepted
> here as "1 ms rounding: TS rounds the file mtime up, Rust truncates. Benign."
> That diagnosis was wrong. It was two float defects in `epoch_ms_to_iso8601` —
> an f64 nanosecond product past 2^53, and `time`'s `Iso8601` renderer emitting
> 38 of every 1000 millisecond values one low. Systematic at ~3.8% of computed
> timestamps rather than a rounding convention. Fixed in
> [#115](https://github.com/vibecook-dev/spaghetti/pull/115); details in the
> handoff §8.
>
> The lesson is about the acceptance itself: "benign rounding" was a plausible
> story that fit the evidence (a 1 ms delta) and closed the question. It took
> reading the actual mtime off disk — exactly `…422.465`, so *one engine was
> simply wrong* — to reopen it. **A divergence explained by a convention
> neither engine documents is a hypothesis, not a finding.**

### Resolved during Phase 5

| Divergence                                | Count | Resolution                                                                                                                                                                                 |
| ----------------------------------------- | ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `subagents.spawn_tool_id` / `link_method` | 226   | Rust never implemented linkage — the columns were filled with `NULL` / `"unlinked"` literals. Ported from TS. **This was data loss**, since RFC 009 deletes the engine that had the links. |

### Resolved on macOS (2026-08-10, [#115](https://github.com/vibecook-dev/spaghetti/pull/115))

Every item left open above is now closed. The macOS corpus (113 projects,
42,783 messages) went from **360 divergences to 223**, and all 223 remaining
are the accepted `agent_type` rows — the documented floor. Every other table
is clean.

| Table / field                         | Rows | Resolution                                                                                                                                                       |
| ------------------------------------- | ---: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `messages.text_content`               |   21 | **Data loss, new on macOS.** `imagePasteIds` typed `Vec<String>` from a TS annotation that says `string[]`; Claude Code writes numbers, so serde failed the whole record and emptied its FTS blob. |
| `subagents.message_count`             |   54 | Same root cause, different call site — the unparseable line is dropped from the transcript. Every diff was exactly `rs = ts − 1`.                                |
| `file_history.data`                   |   50 | Snapshots emitted in `read_dir` order. NTFS sorts, APFS does not — 1 row on Windows, 50 here. Now sorted by file name.                                           |
| `sessions.summary`                    |    6 | `String` collapsed *absent* (TS omits the key → `NULL`) and *empty* (TS writes `''`). Now `Option<String>` with `skip_serializing_if`.                          |
| `projects.sessions_index`             |    4 | 3 were the `summary` bug above; the 4th was the timestamp bug below.                                                                                            |
| `sessions.created_at` / `modified_at` |    2 | Two float defects in `epoch_ms_to_iso8601` — see the retraction above.                                                                                          |
| `plans.title`                         |    0 | Did not reproduce — closed by the CRLF fix in #111, and a macOS corpus is pure-LF.                                                                              |
| `tool_results`                        |    0 | Did not reproduce — the row needed an `E--` Windows drive-root slug. 143 rows clean.                                                                             |

### Resolved — silent typed-parse failure ([#116](https://github.com/vibecook-dev/spaghetti/pull/116))

Not a divergence in the diff, which is why five phases missed it: a sweep of
the corpus through the Rust typed parser found **5,035 of 89,559 session lines
(5.6%) failing to deserialize**, every one silently. Both call sites discarded
the error (`Err(_) => None`, `if let Ok(msg)`), so a failure was
indistinguishable from a record that legitimately contributes no text.

Three of the causes are fixed outright — `imagePasteIds` and
`attachment.content` (3,754 lines) in #115, `LastPromptMessage` (515 lines) in
#116, all three the same defect of a shape asserted from documentation rather
than from data. That leaves 745, which are not `SessionMessage`s at all:
skills/context telemetry keyed on `event` rather than `type`.

**The systemic fix is the reporting, and it could not simply report
everything.** `errored_paths` gates fingerprint withholding, so reporting all
5,035 would withhold the fingerprint of every file containing one and re-ingest
a large fraction of the corpus on every warm start, forever — against a warm
path measured in §3 at 60 ms. The filter is therefore whether the failure cost
anything: the `type` discriminator is read off the raw JSON (which still works
when the typed parse just failed) and reported only when that variant would
have contributed searchable text. On this corpus that is false for all 1,260
remaining failures, so the error report is empty and warm start is untouched.

Verified by breaking one `user` line and watching the alarm ring — a
check that is silent when it should shout cannot be trusted on "tests pass":

```
cold  projects=114 errorCount=0     no false alarms
warm  projects=0                    still a true no-op
break one user line, warm again:
      projects=9  errorCount=1
      [record-skip] session … line 0: stored without searchable text —
                    type=user: missing field `role`
warm again: still re-reads, because the fingerprint stays withheld
```

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

**Re-proven on the published `0.6.2`, 2026-08-11** (originally on `0.6.1`) —
not on a branch, and against a sandboxed `HOME` so the real cache was never
touched. Same result on both releases, `native: available (v0.6.2)`:

| Step                                     | Result                                                                    |
| ---------------------------------------- | ------------------------------------------------------------------------- |
| `SPAG_ENGINE=ts spag engine`             | `engine: ts (TypeScript)`, `source: env SPAG_ENGINE=ts`                   |
| `SPAG_ENGINE=ts spag projects`           | TS engine rebuilt its own index into `spaghetti-ts.db`                    |
| `spaghetti-rs.db` after the TS run       | **byte-identical** (md5 unchanged) — no cross-engine contamination        |
| Totals, both engines                     | identical: 9 projects · 32 sessions · 712 messages · 996.2K tokens        |
| `spag engine ts` → `spag engine`         | `config.json` written; `source: config file`                              |
| `spag engine rs` (round trip)            | switches back; both DB files coexist, neither rebuilt                     |

The TypeScript engine remains selectable throughout.

### The data-loss fix, confirmed in the published artifact

Verified on shipped tarballs rather than a build — a session carrying one user
message of the shape that used to fail (`imagePasteIds: [1]`, content array of
`text` + `image`), searched through the CLI:

| Release | `spag search "ZANZIBARQUUX"`     |
| ------- | -------------------------------- |
| `0.6.1` | `No results` — silently unindexed |
| `0.6.2` | `1 results` ✅                    |

Both engines find it on `0.6.2`, so the recovered text is in the index rather
than in one engine's projection of it. This is the check worth repeating on any
future release: the failure it guards against moved no counter and raised no
error — the row was present and its text simply was not.

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

| #   | Gate                                                  | Status                                                                             |
| --- | ----------------------------------------------------- | ---------------------------------------------------------------------------------- |
| 1   | Publish a minor release with the TS fallback retained | ✅ `0.6.0` → `0.6.1` → `0.6.2` (2026-08-11) carrying every fix below               |
| 2   | Resolve or explicitly accept the open divergences     | ✅ all closed on two platforms; only accepted `agent_type` remains (§6)            |
| 3   | Prove rollback on that release                        | ✅ re-proven on published `0.6.2` (§8)                                             |
| 3b  | The data loss is actually gone from the artifact      | ✅ `0.6.1` returns no results, `0.6.2` finds the message (§8)                      |
| 4   | A stock install of that release runs                  | ❌ **fails on npm 12** — the one gate still open                                   |
| 5   | Re-date and sign                                      | ⚠️ **withheld** — see below                                                        |

**Why sign-off is still withheld.** Gates 1–3b are now genuinely met on a
published artifact, and the data loss is demonstrably fixed for users. Gate 4
is not, and it is the one the completion criterion is really about — *"the
shipped artifact works."*

`npm install -g @vibecook/spaghetti@0.6.2` on npm 12, the current npm, still
produces a CLI where every command that touches the index fails: npm 12 blocks
install scripts by default and `better-sqlite3` fetches its binding from a
postinstall. `0.6.2` improves on `0.6.1` by *diagnosing* this correctly instead
of blaming a missing agent — but it does not remove the dependency on that
script, so a stock install is still broken.

Worse, and found only by running them: **the two remedies `0.6.2` prints were
themselves wrong**, in both the CLI message and the README. `--allow-scripts` is
refused outright for project-scoped installs, and `npm install-scripts approve`
records the permission without ever executing the script — so it answers
"Approved" and changes nothing. A user who followed either was left exactly as
broken, by a tool that now sounded certain.
[#117](https://github.com/vibecook-dev/spaghetti/pull/117) replaces them with
forms that were each executed against the published tarball first.

That leaves a real decision rather than more work, and it is not the report's
to make:

- **Ship `0.6.3` with #117 and sign**, accepting that installation takes one
  documented step on npm 12. Defensible: the step is small, correct, and
  discoverable from the error itself.
- **Or treat "a stock `npm install` must work" as the gate** and remove the
  postinstall dependency first — `node:sqlite`, or a binding shipped as real
  npm dependencies. Larger, and RFC-sized.

Either way `0.6.2` should not be the release that gets signed: its install
guidance is actively wrong, and a shipped release cannot be repaired in place.

Remaining before sign-off:

1. ~~Merge #115, then #116.~~ **Done 2026-08-11** — `456bf28` and `4df448c`.
   Release-please regenerated
   [#112](https://github.com/vibecook-dev/spaghetti/pull/112) within a minute of
   each, so `0.6.2` now carries both. That ordering was the one intervention
   that mattered: had #112 merged first it would have spent the version number
   on a build that still dropped user text from search, and a shipped release
   cannot be repaired in place.
2. ~~Release `0.6.2`, then verify a clean `npm install` on npm 12.~~
   **Done 2026-08-11, and the install gate failed** — see the table above. The
   release shipped; the stock install did not work, and the guidance it printed
   was wrong.
3. ~~Re-run the fixture diffs and one real-corpus audit against that release.~~
   **Done** — four fixtures zero, real corpus 223 (all accepted `agent_type`),
   rollback re-proven, and the data loss confirmed gone from the published
   artifact (§8).
4. **Merge [#117](https://github.com/vibecook-dev/spaghetti/pull/117) and cut
   `0.6.3`** so the printed remedies are ones that work.
5. Decide the question in the table above: is "installs in one documented step"
   sufficient, or must a stock `npm install` work? Only the second needs more
   engineering.
6. Re-date this report and sign §9.

Until then RFC 009 may **not** begin Phase 0. Every other handoff condition is
met: the token-estimation policy is settled, the supported-platform list is
published and now verified against the published tarball, `EngineUnavailableError`
has shipped, the warm strategy is measured and recorded, rollback is proven on a
real release, and no unresolved divergence involves stale warm results or
partial project commits.
