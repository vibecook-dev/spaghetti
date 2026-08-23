# Lane L8 — docs: trim the child RFCs to what shipped, release notes, downstream notes

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l8-docs`, branch `land/l8-docs`.
Read `COMMON.md` first (rules 7 and the reporting format apply; no cargo/pnpm
needed except `pnpm format:check`/`pnpm lint` if you touch code comments).
Base: `main` ≥ `6d91eef`. Docs-only lane: you may not change code, fixtures,
or generated files; links and doc comments only.

## Outcome

A reader (James, a Chopsticks or VibeField engineer, a new contributor) can
learn what RFC 012 delivers and how to use it from ≤ 1,500 lines of current
docs instead of 12,800 lines of program history — without losing the history.

## Work

1. **Trim RFC 012B, 012C, 012D** (`docs/rfcs/012b-*.md` 699 lines, `012c-*.md`
   1,304, `012d-*.md` 919) to ≤ 300-line semantic contracts each: decisions,
   the semantic rules that the code now enforces, the public interface as
   shipped (point to generated types in `packages/sdk/src/generated/`, the
   napi class/methods in `crates/spaghetti-napi/index.d.ts`, the SDK functions
   in `packages/sdk/src/index.ts`), and acceptance tests (point to the actual
   test files: `observer/tests/*`, `engine/catalog/tests.rs`, `engine/usage_query/tests.rs`,
   `packages/sdk/src/__tests__/{observe-session,vibefield,observation-host}.test.ts`,
   `scripts/usage_v2_oracle`). Sections describing the provisional wire/API
   shapes that the landing replaced become one line: "superseded by <pointer>".
   Keep each child's `Status:` honest: "Implemented (landing 2026-08-23);
   ratification pending owner review" — do not ratify. Preserve the full
   previous text by `git mv` into `docs/rfcs/archive/` before trimming
   (e.g. `archive/012d-session-scoped-observation-2026-08-15-draft.md`).
2. **Umbrella RFC 012 and 012A**: leave normative text alone (ratified); add a
   short "Landing status (2026-08-23)" section at the top pointing to
   `012-landing-plan.md` §3/§8 and listing which umbrella decisions are now
   implemented and where; fix links broken by the archive moves.
3. **Archive** with `git mv` (update every inbound link): `012-implementation-plan.md`,
   `012-implementation-deduplication-plan.md`, `012-parallel-work-handoff.md`,
   `012-wave-i-execution.md`, `012-wave-iii-execution.md`, the four census/audit
   reports (`012-phase-0-census-2026-08-15.md`, `012-runtime-observation-census-2026-08-15.md`,
   `012-rfc011-delta-evidence-audit-2026-08-17.md`), and
   `012-system-diagrams.html` if tracked. Keep `012-landing-plan.md` and
   `012-landing-lanes/` where they are. Move the one-shot census scripts
   (`scripts/catalog_census`, `runtime_observation_census`, `diagnostic_census`,
   `team_affiliation_census`, `rfc012_experiments`) under `scripts/archive/`
   **only if** nothing in `package.json`, `validate-all.sh`, or CI references
   them — if something does, list it instead (code changes are not yours).
4. **README.md**: catalog-first startup (library in < 1 s, readiness vector,
   `spag doctor`), corrected usage (response-level; totals ~2× lower than
   0.7.x and why), `observeSession` for live consumers (link the SDK README
   section), `watchSessionTranscript` deprecated with a one-release overlap.
5. **CHANGELOG.md** draft entry for 0.8.0 (release-please will generate the
   commit list; write the human notes block): BREAKING — usage totals corrected
   (cite the 2.13× and the reason); schema v64 forces a full rebuild at first
   start (catalog appears immediately; history/search converge in the
   background; on a large corpus this can take hours until L7 lands — say so
   honestly); SDK barrel curated (list the removed public exports from the
   `a0bc677` allowlist commit); new: `observeSession`, `getReadiness`,
   catalog-first `listProjects/listSessions` fields; deprecated:
   `watchSessionTranscript`.
6. **Downstream notes**: `docs/integration/chopsticks-observe-session.md`
   (migration from `watchSessionTranscript`: request shape, event families
   available today — only the families the Claude adapter emits; the rest
   arrive when L5 lands — epochs/resync handling, close/abort) and
   `docs/integration/vibefield-phase-a.md` (the §3.2 surface: `SessionRef`/
   `ProjectRef`, `at_commit_seq`, `SemanticRevisionRef`, readiness, observer
   epochs; with the generated type names and one example each). Add a
   "Status vs landing" section to `docs/petition/vibefield-needs.md` mapping
   Phase A to the shipped surface.
7. `pnpm format:check` and `pnpm lint` green (markdown is not linted here, but
   code comments you touch are); every moved file has zero dangling inbound
   links (`grep -rn` the old paths across docs/, README, packages/, scripts/).

## Ownership / conflicts

Docs only. L5/L6/L7 may edit code comments and `packages/sdk/README.md`
sections about `SemanticEvent.value` — if you touch that README, keep to the
migration section and expect a trivial merge.
