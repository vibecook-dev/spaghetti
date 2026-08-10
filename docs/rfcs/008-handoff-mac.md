# RFC 008 — Handoff for cross-platform verification

**Written:** 2026-08-10, from a Windows machine · **Audience:** whoever picks this up on macOS or Linux
**Read first:** [readiness report](./008-readiness-report.md) · [RFC 008](./008-rust-ingest-production-readiness.md)

Phases 0–5 are merged and the soak release is published (`0.6.1`). RFC 008 is
**not signed off**. This note is what a fresh pair of hands needs to finish it.

> ## ✅ Executed on macOS, 2026-08-10 — see [§8 Outcome](#8-outcome-macos-run)
>
> Fixes in **[#115](https://github.com/vibecook-dev/spaghetti/pull/115)** and
> **[#116](https://github.com/vibecook-dev/spaghetti/pull/116)**. On a
> 113-project real corpus the audit went from **360 divergences to 223**, and
> all 223 are the accepted `agent_type` rows. Every other table is clean.
>
> The core prediction below held: macOS found a **different** set, not a
> subset. Both open Windows items vanished, and three classes Windows never
> showed turned up — one of them silent data loss in `messages`.
>
> Sign-off is still withheld, but for a **new** reason: the published `0.6.1`
> cannot be installed on npm 12. See §8.

---

## 1. Why a second platform matters here, specifically

This is not a box-ticking exercise. **Every divergence class found so far has
been platform-dependent**, and all the work to date ran on Windows:

| Bug                                             | Why Windows hid or caused it                                                                                                                                                                                              |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Merged session index disagreed between engines  | Neither engine sorted discovered entries. NTFS returns directory entries sorted, ext4 and APFS do not — so the two engines agreed on Windows and disagreed everywhere else. Found only when CI failed on Linux and macOS. |
| Plan titles differed by one invisible character | The files were CRLF. JS treats CR as a line terminator for `(?m)$`; Rust's regex crate recognises only LF. A pure-LF corpus would never show it.                                                                          |
| A `tool_results` row missing in Rust            | The project slug is `E--` — a Windows drive root (`E:\`). Very likely will not reproduce on macOS at all.                                                                                                                 |

Expect the Mac run to find a **different** set, not a subset. Also expect some
open items below to disappear there, which is itself information: a divergence
that only exists on one platform is a path-handling or line-ending bug, and
that narrows the search enormously.

---

## 2. Open divergences, with leads

> **All closed as of 2026-08-10 — see [§8](#8-outcome-macos-run).** The table
> below is the Windows-side state and the leads that were handed over; two of
> the four did not reproduce on macOS, which was itself the diagnosis.

Run the real-corpus audit (§4) and compare against this list.

| Divergence                            | Status and lead                                                                                                                                                                                                                                                                                                                                               |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `tool_results` — Rust missing 1 row   | **Best-understood.** TS writes 17 rows, Rust 16. It is _not_ four field bugs: the harness compares positionally, so one absent row shifts everything after it. The missing row is `project_slug='E--'`, a Windows drive root. Look at Rust's tool-results discovery for a path-shape assumption that breaks on a bare drive root. May not reproduce on macOS. |
| `file_history.data` (1 row)           | Unexamined. `data` is a content blob read from a file — **check CRLF first**, given the plan-title precedent.                                                                                                                                                                                                                                                 |
| `sessions.created_at` / `modified_at` | Timestamps differ by 1 ms. Rust's `mtimeMs` arithmetic was already aligned to Node's (`sec*1000 + nsec/1e6`) and it did **not** move the count, so the cause is elsewhere. Suspect the ISO rendering path or the index-entry vs discovered-entry distinction.                                                                                                 |
| `projects.sessions_index` (1 row)     | Both engines now sort discovered entries, which closed most of these. One row still differs; it may be a different cause entirely (contents rather than order).                                                                                                                                                                                               |

### Accepted — do not "fix"

- **`subagents.agent_type`, ~412 rows.** Rust is **correct**; TS is wrong. TS
  records the literal `"task"`; Rust reads the `agent-*.meta.json` sidecar and
  records the real type (`general-purpose`, `Explore`, `Plan`, …). RFC 009
  deletes the TS engine, so backporting would be work on a corpse. **This means
  the real-corpus diff can never reach zero while both engines exist**, so it
  stays a manual audit and cannot become a CI gate.

---

## 3. Non-divergence work remaining

> **Status after the macOS run (§8):** (1) done — rollback proven on published
> `0.6.1`. (2) done — §1 now lists the shipped versions. (3) **still withheld**,
> and the reason changed: the divergences are closed, but `0.6.1` cannot be
> installed on npm 12, and the fixes in #115 are unreleased. RFC 009 stays
> blocked.

1. **Prove rollback on the published release.** The readiness report calls for
   it and it has not been done. Install `0.6.1`, force the TS engine
   (`SPAG_ENGINE=ts`), confirm the index rebuilds and the per-engine cache
   files stay separate. This is a completion-gate item, not a nice-to-have.
2. **Fill in readiness report §1** with the shipped versions, now that `0.6.0`
   and `0.6.1` exist.
3. **Re-date and sign §9** once the above and §2 are closed. Until then
   **RFC 009 may not begin.**

---

## 4. How to run things

```bash
pnpm install
cd crates/spaghetti-napi && pnpm build && cd -   # release addon; required by the diff harness

# Cross-engine parity — all four must stay at zero
pnpm test:ingest-diff            # small (Claude)
pnpm test:ingest-diff:medium
pnpm test:ingest-diff:grok
pnpm test:ingest-diff:codex

# The real-corpus audit that found everything interesting
INGEST_DIFF_SHOW=2000 npx tsx scripts/ingest-diff.ts --fixture ~/.claude

# Aggregate by table+field instead of reading raw rows (which dump real content)
INGEST_DIFF_SHOW=2000 npx tsx scripts/ingest-diff.ts --fixture ~/.claude 2>&1 \
  | grep -oE "\[[a-z_]+#[0-9]+\] field field=[a-z_]+" | sed -E 's/#[0-9]+//' \
  | sort | uniq -c | sort -rn

cargo test -p spaghetti-napi --lib     # 232 tests, incl. the Phase 1/2 matrices
pnpm --filter @vibecook/spaghetti-sdk test
```

**Rebuild the addon after any Rust change** before running `ingest-diff` — it
loads the built binary, not the source. Forgetting this produces a "fix that
didn't work" that actually did.

### Benchmarks

```bash
node scripts/generate-medium-fixture.mjs --out /tmp/large --scale 50
pnpm bench:ingest --fixture /tmp/large/.claude --mode warm --scenario growth --runs 5 --warmup 1
```

`--scenario` is `unchanged | growth | deletion | repair`. Mutating scenarios
copy the fixture first, so they cannot touch a real `~/.claude`.

---

## 5. Traps in this repo

- **`~/.claude` is live.** The real-corpus counts move between runs because the
  corpus grows while you audit it. Treat numbers as snapshots and _shapes_ as
  findings. Only the fixtures are reproducible enough to gate CI.
- **Never run `pnpm rebuild better-sqlite3`.** It deletes the working prebuilt
  `.node` before failing. Keep the floor at `^12.11.1`, which has a Node 26
  prebuild.
- **`crates/spaghetti-napi/npm/*` is vestigial.** Those platform packages are
  frozen at `0.5.18` and nothing resolves them — the native package ships every
  binary bundled (`files: ["*.node"]`, no `optionalDependencies`). **I already
  made the mistake of "fixing" a missing musl package there; it was reverted.**
  Don't repeat it. If `npm view <platform-pkg>` says not-found, that is normal.
- **`pnpm validate` can't run its real-data half in CI.** Use
  `pnpm test:packages` as the clean signal.
- **A test-only fault seam exists** at `claude::project_parser::fault` for
  failures with no portable filesystem provocation. Prefer a real fault where
  one exists — a _file standing where a directory belongs_ makes `read_dir`
  fail with a non-`NotFound` error on all three platforms.
- **Scope prettier to the files you touched.** `scripts/` is not in
  `format:check`, and a broad glob reformats unrelated files. (`README.md` is
  outside the glob and already fails `prettier --check` on its tables — leave
  it alone rather than reformatting it as a side effect.)
- **npm 12 blocks install scripts**, so a stock `npm i @vibecook/spaghetti`
  leaves `better-sqlite3` without its binding and every DB command dies. The
  repo is immune because `pnpm-workspace.yaml` lists it under
  `onlyBuiltDependencies` — which is exactly why this went unnoticed until
  someone installed the published tarball. Added 2026-08-10; see §8.
- **Freeze the corpus before auditing.** `cp -Rc ~/.claude <snapshot>` is an
  APFS clone — seconds, no meaningful extra disk — and it removes the
  "counts move between runs" problem §5 warns about. Without it you cannot
  tell a fix from corpus drift. On a big corpus, subset it too: the full one
  produced a >7 GB DB *per engine*.

---

## 6. The pattern worth internalising

Three defects in three phases hid the same way: **a fixture that passed while
covering nothing.**

- Codex fixtures emitted turns as `event_msg`, which the extractor skips. Six
  sessions produced five messages and **zero turns**, so the token-attribution
  fixtures tested no attribution.
- The `small` fixture's subagent was never referenced by any `tool_result`, so
  `unlinked` was the _correct_ answer in both engines — which is why nobody
  noticed Rust hardcoded `unlinked` and never implemented linkage at all.
- The bench repair scenario used an invalid column name inside a `try/catch`,
  so it silently did nothing and reported fast-path timings as a forced
  rebuild.

**Zero rows and correct rows both look like a passing test.** When a fixture is
built from a format's documentation rather than from real output, assert the
_row count_ it produces, not just that ingest succeeded. And when a check
swallows errors, make it loud — silence is indistinguishable from success.

This is also the argument for the real-corpus audit: it found in one run what
five phases of fixtures did not.

---

## 7. What I'd do first

1. Run the four fixture diffs. They should be zero on macOS; if not, that is a
   platform bug and more interesting than anything in §2.
2. Run the real-corpus audit and diff the table/field aggregate against §2.
   **Items that vanish are as informative as items that remain.**
3. Take `tool_results` if it reproduces; otherwise `file_history.data`, testing
   the CRLF hypothesis first.
4. Prove rollback on `0.6.1` — it is a gate item and needs no investigation.

---

## 8. Outcome (macOS run)

**Run:** 2026-08-10, macOS 15 / APFS / arm64, Node 26.5.0, corpus of 113
projects · 260 sessions · 42,783 messages. Fixes: **[#115](https://github.com/vibecook-dev/spaghetti/pull/115)**.

Fixture diffs were zero on macOS before any change, so there was no
platform bug at the fixture layer.

### The §2 table, resolved

| §2 item                               | Outcome                                                                                                                         |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `tool_results` — Rust missing 1 row   | **Did not reproduce.** 143 rows clean. The `E--` drive-root hypothesis was right — it is Windows-only.                          |
| `plans.title`                         | **Did not reproduce.** 36 rows clean; the CRLF fix in #111 holds and a macOS corpus is pure-LF anyway.                          |
| `file_history.data`                   | **Reproduced and grew** — 1 row on Windows, 50 here. Not CRLF: snapshots were emitted in `read_dir` order. Fixed by sorting.    |
| `projects.sessions_index`             | **Reproduced.** 3 of 4 were the `summary` null/empty bug below; the 4th was the timestamp bug. All closed.                      |
| `sessions.created_at` / `modified_at` | **Reproduced, and the report's explanation was wrong.** Not rounding direction — two float bugs in the formatter. See below.    |

### Three classes Windows never showed

| Divergence                  | Rows | Cause                                                                                                                                                        |
| --------------------------- | ---: | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `messages.text_content`     |   21 | **Data loss.** `imagePasteIds` is typed `string[]` in TS but Claude Code writes *numbers*. Rust mirrored the annotation, serde rejected the whole record. |
| `subagents.message_count`   |   54 | Same root cause, different call site: the unparseable line is dropped from the transcript instead of blanked. Every diff was exactly `rs = ts − 1`.          |
| `sessions.summary`          |    6 | `String` collapsed *absent* (TS omits the key, writes `NULL`) and *empty* (TS writes `''`). The corpus has 6 and 117.                                        |

### What this says about the engine

The `imagePasteIds` bug is the one worth internalising, and it generalises §6:

- **A wrong TS type annotation is free in TS and fatal in Rust.** TypeScript
  erases types at runtime, so `string[]` on a field that holds numbers cost the
  TS engine nothing for as long as it has existed. Porting it faithfully turned
  a documentation error into silent data loss.
- **The damage lands nowhere near the wrong field.** `imagePasteIds` is never
  read. It failed the record, and the *record's* text vanished from search.
- **Both call sites swallow the error** (`Err(_) => None`,
  `if let Ok(msg)`), so nothing was reported, no count moved, and the row was
  still written — just empty. A sweep of the corpus through the typed parser
  found **5,035 of 89,559 session lines (5.6%) failing silently**, dominated by
  `attachment.content` (3,754), which is typed `String` but is sometimes an
  array. That one is currently harmless — attachments contribute no FTS text —
  but it is one call site away from mattering, and #115 fixes it too.

**RFC 008 §9 lists "silent parser failure" as a sign-off blocker.** It was
happening on 5.6% of lines the whole time. Making a typed-parse failure *loud*
is the systemic fix, and it landed in
[#116](https://github.com/vibecook-dev/spaghetti/pull/116).

The interesting part is that it could not simply report everything.
`errored_paths` gates fingerprint withholding, so reporting all 5,035 failures
would have withheld the fingerprint of every file containing one and re-ingested
much of the corpus on every warm start, forever — against a 60 ms warm path. The
filter is whether the failure cost anything: read the `type` discriminator off
the raw JSON (still available when the typed parse just failed) and report only
when that variant would have contributed searchable text. False for all 1,260
remaining failures here, so the report is empty and warm start is untouched —
while the `imagePasteIds` bug that started this would have been caught the first
time it ran.

### The timestamp bug was mischaracterised

The report called this "1 ms rounding: TS rounds the file mtime up, Rust
truncates. Benign." Both halves are wrong. `epoch_ms_to_iso8601` carried two
independent float defects:

1. `ms * 1e6` exceeds f64's exact-integer range (2^53 ≈ 9.0e15). A 2026 mtime
   is ~1.79e12 ms, so the nanosecond product is ~1.79e18 and the nearest f64
   lands low — `1785895422465.0` ms became `1785895422464999936` ns, and
   truncating dropped a whole millisecond.
2. Underneath it, `time`'s `Iso8601` config asked for 3 decimal digits renders
   **38 of every 1000** millisecond values one low — exactly those ≡ 4 or 7
   (mod 8), the fractions not exactly representable in binary.

So it was systematic at ~3.8% of timestamps, not a rounding convention. It
surfaced on only one session here because indexed sessions copy their
timestamps verbatim from `sessions-index.json`; only *discovered* sessions
compute from mtime. A corpus with many unindexed sessions would show far more.

The fraction is now formatted from the integer remainder, and the regression
test sweeps all 1000 fractions — every single-value test already in that file
passed straight through the bug.

### Non-divergence gate items

- **Rollback proven on the published `0.6.1`** (§3.1), in a sandboxed `HOME`:
  `SPAG_ENGINE=ts` reports `engine: ts (TypeScript), source: env`, the TS
  engine rebuilds its own index, and `spaghetti-rs.db` comes back
  **byte-identical** (md5) afterwards. The persisted path (`spag engine ts` →
  `config.json` → `source: config file`) and the round-trip back to `rs` both
  work. Both engines report identical totals.
- **The published `0.6.1` cannot be installed on npm 12.** npm 12 blocks
  install scripts by default, so `better-sqlite3` never fetches its prebuilt
  binding and every DB-touching command dies with `Could not locate the
  bindings file` — reported to the user as *"Install a supported agent and
  re-run"*, which is the wrong advice for someone whose agent is installed.
  #115 fixes the diagnostic and documents `--allow-scripts=better-sqlite3`.
  **This is why §9 is still unsigned:** the soak gate exists to prove the
  shipped artifact works, and this one does not on current npm.

### Method note

The audit ran against an **APFS clone of `~/.claude`** (`cp -Rc`, seconds, no
meaningful extra disk) rather than the live directory. §5 warns that live
counts move between runs; freezing a snapshot removes that entirely and is
what made "measure, fix, re-measure" trustworthy — each fix was verified
against byte-identical input. Worth doing on any platform.

The full corpus was subsetted to 113 of 136 projects (the 23 excluded are the
largest). The whole corpus produced a >7 GB DB per engine and the machine had
19 GB free; the subset keeps 83% of project diversity in 8% of the bytes. Since
the harness compares engine-vs-engine on identical input, any subset is a valid
experiment.

### Still open

- `subagents.agent_type`, 223 rows — accepted, unchanged, and still the reason
  the real-corpus diff can never reach zero while both engines exist.
- **Ship it, in this order.** #115 and #116 are unreleased, and `0.6.1` cannot
  be installed on npm 12. Sign-off waits on a `0.6.2` that a stock
  `npm install` can actually run — verify that as part of cutting it, because
  the workspace cannot detect this class of breakage (pnpm allowlists
  `better-sqlite3`). **Watch the release PR:** release-please already has
  [#112](https://github.com/vibecook-dev/spaghetti/pull/112) open for `0.6.2`,
  raised before this work and carrying only #111. Merging it ahead of the fixes
  spends the version number on a build that still drops user text from search,
  and a shipped release cannot be repaired in place. Merge the fixes first and
  let #112 regenerate.
- Re-run on **Linux/ext4**. Two of the five bugs were filesystem- or
  platform-shaped, so a third platform is still worth a pass — though the
  ordering fix now pins the one that was FS-dependent.
- The 745 lines that still fail the typed parse are skills/context telemetry
  keyed on `event` rather than `type` — a different record family that is not a
  `SessionMessage` at all. They cost nothing today. If they ever need indexing
  they need their own reader, not another variant bolted onto this enum.
