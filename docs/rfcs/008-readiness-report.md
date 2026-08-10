# RFC 008 — Rust Ingest Readiness Report

**Status:** ⚠️ **Draft — not signed off.** The data-divergence and rollback
gates are now met on two platforms, but the published soak release
(`0.6.1`) **cannot be installed on npm 12**, so it does not demonstrate a
working shipped artifact. Sign-off waits on a release that can. See §9.

**Cross-platform verification:** [`008-handoff-mac.md`](./008-handoff-mac.md) — executed on macOS 2026-08-10 (§8 of that doc). Fixes in [#115](https://github.com/vibecook-dev/spaghetti/pull/115). As predicted, macOS found a *different* divergence set, not a subset: both open Windows items vanished and three new classes appeared, one of them silent data loss.

**Dated:** 2026-08-10 · **Verified on:** Windows 11 / NTFS and macOS 15 / APFS · **Current version:** `0.6.1` (published)
**Phases:** [0](./008-phase-0-baseline.md) · [1](./008-phase-1-gate.md) · [2](./008-phase-2-gate.md) · [3](./008-phase-3-gate.md) · [4](./008-phase-4-gate.md)

---

## 1. Shipped package versions and native target matrix

Versions are lockstep across `@vibecook/spaghetti-sdk`,
`@vibecook/spaghetti-sdk-native`, and the CLI.

**The soak release is cut.** `0.6.0` shipped the Rust behaviour with the TS
fallback retained; `0.6.1` followed with the CRLF plan-title fix. All three
packages are published at `0.6.1` and `latest` points at it:

| Package                          | Published | Notes                                                          |
| -------------------------------- | --------- | -------------------------------------------------------------- |
| `@vibecook/spaghetti`            | `0.6.1`   | CLI                                                            |
| `@vibecook/spaghetti-sdk`        | `0.6.1`   |                                                                |
| `@vibecook/spaghetti-sdk-native` | `0.6.1`   | ships all 8 platform binaries bundled; `next` still on `0.6.0-rc.0` |

The native package carries every target in the one tarball (`files: ["*.node"]`,
no `optionalDependencies`) — verified by unpacking the published artifact, which
also confirms the `crates/spaghetti-napi/npm/*` platform packages are vestigial
(see the handoff's trap list).

**Caveat that blocks sign-off:** the published CLI does not work on a stock
npm 12 install. See §9.

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

### Open — silent typed-parse failure

Not a divergence in the diff, which is why five phases missed it: a sweep of
the corpus through the Rust typed parser found **5,035 of 89,559 session lines
(5.6%) failing to deserialize**, every one silently. Both call sites discard
the error (`Err(_) => None`, `if let Ok(msg)`) and neither reports it, so a
failure is indistinguishable from a record that legitimately contributes no
text.

Two of those causes are fixed in #115 (`imagePasteIds`, and
`attachment.content` at 3,754 lines — typed `String` but sometimes an array).
The remainder are variants that contribute no text today, so they cost nothing
*yet*. **The systemic fix — routing a typed-parse failure to the `record-skip`
error sink §5 already defines — is not done**, and RFC 008 §9 names "silent
parser failure" as a sign-off blocker. This is the first thing to pick up next.

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

**Proven on the published `0.6.1`, 2026-08-10** — not on a branch. Run against
a sandboxed `HOME` so the real cache was never touched:

| Step                                     | Result                                                                    |
| ---------------------------------------- | ------------------------------------------------------------------------- |
| `SPAG_ENGINE=ts spag engine`             | `engine: ts (TypeScript)`, `source: env SPAG_ENGINE=ts`                   |
| `SPAG_ENGINE=ts spag projects`           | TS engine rebuilt its own index into `spaghetti-ts.db`                    |
| `spaghetti-rs.db` after the TS run       | **byte-identical** (md5 unchanged) — no cross-engine contamination        |
| Totals, both engines                     | identical: 9 projects · 32 sessions · 712 messages · 996.2K tokens        |
| `spag engine ts` → `spag engine`         | `config.json` written; `source: config file`                              |
| `spag engine rs` (round trip)            | switches back; both DB files coexist, neither rebuilt                     |

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

| # | Gate                                                | Status                                                                        |
| - | --------------------------------------------------- | ----------------------------------------------------------------------------- |
| 1 | Publish a minor release with the TS fallback retained | ✅ `0.6.0`, then `0.6.1`                                                       |
| 2 | Resolve or explicitly accept the open divergences     | ✅ all closed on two platforms; only accepted `agent_type` remains (§6)        |
| 3 | Prove rollback on that release                        | ✅ proven on published `0.6.1` (§8)                                            |
| 4 | Re-date and sign                                      | ⚠️ **withheld** — see below                                                    |

**Why sign-off is withheld even though 1–3 are met.**

Gate 1 is satisfied only in the narrow sense that a version number exists on
npm. `npm install -g @vibecook/spaghetti@0.6.1` on npm 12 — the current npm —
produces a CLI where every command that touches the index fails, because npm 12
blocks install scripts by default and `better-sqlite3` fetches its native
binding from a postinstall. The soak gate exists to prove the *shipped artifact*
works. This one does not.

Two further items, neither of which existed when this report was first drafted:

- The parity fixes in
  [#115](https://github.com/vibecook-dev/spaghetti/pull/115) — including the
  `messages.text_content` **data loss** — are unreleased. Signing off on
  `0.6.1` would sign off on a build that silently drops user text from search.
- **Silent parser failure**, which §9 names as a blocker, is real and
  measured: 5.6% of session lines fail the typed parse without reporting
  anything (§6). #115 fixes the two causes that had visible consequences; the
  systemic fix does not exist yet.

Remaining before sign-off:

1. Merge #115.
2. Route typed-parse failures to the `record-skip` error sink so the failure
   mode is loud (§6).
3. Cut **`0.6.2`**, and verify a clean `npm install` of it works on npm 12.
4. Re-run the fixture diffs and one real-corpus audit against that release.
5. Re-date this report and sign §9.

Until then RFC 009 may **not** begin Phase 0. Every other handoff condition is
met: the token-estimation policy is settled, the supported-platform list is
published and now verified against the published tarball, `EngineUnavailableError`
has shipped, the warm strategy is measured and recorded, rollback is proven on a
real release, and no unresolved divergence involves stale warm results or
partial project commits.
