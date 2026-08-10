# RFC 008 — Handoff for cross-platform verification

**Written:** 2026-08-10, from a Windows machine · **Audience:** whoever picks this up on macOS or Linux
**Read first:** [readiness report](./008-readiness-report.md) · [RFC 008](./008-rust-ingest-production-readiness.md)

Phases 0–5 are merged and the soak release is published (`0.6.1`). RFC 008 is
**not signed off**. This note is what a fresh pair of hands needs to finish it.

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
  `format:check`, and a broad glob reformats unrelated files.

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
