# RFC 012 Wave I execution runbook

- **Status:** SUPERSEDED on 2026-08-23 by
  [012-landing-plan.md](../012-landing-plan.md). Kept for history; do not
  update. The RFC documents remain the semantic authorities.
- **Written:** 2026-08-21
- **Product-code predecessor:** `aa9b15d`
- **Assignment base:** the integrator commit that lands this file; announced as
  `RFC012 WAVE I BASE <sha>`
- **Supersedes:** Wave 2+ of
  [012-parallel-work-handoff.md](./012-parallel-work-handoff.md)
- **Does not reuse:** stale SSD worktrees `a1-c1`, `a2`, `a3`, `b2`, `c2`, `c3`

Normative semantics remain RFC 012 / 012A–D and
[012-implementation-plan.md](./012-implementation-plan.md). This file is
operational only.

## 1. Slots

Three implementation lanes plus one integrator. Subagents never merge and never
promote. The integrator reviews, validates, and merges in **C2 → A3 → B2**
order.

| Role | Worktree | Branch |
| --- | --- | --- |
| Integrator | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/root` | `work/rfc012-integration` |
| Lane C2 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w1-c2` | `work/w1-c2` |
| Lane A3 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w1-a3` | `work/w1-a3` |
| Lane B2 | `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/w1-b2` | `work/w1-b2` |

```bash
export CARGO_TARGET_DIR=/Volumes/SamsungRed/spaghetti-rfc012/build/cargo-target
```

Do not `cargo clean`. Do not prune other worktrees. Do not copy `draft/`.

## 2. Checkpoints

1. Read-only freeze: objective, exact paths, invariants, negatives, validation.
   Wait for `GO <lane> <frozen paths>`.
2. Compile + focused tests, still unstaged.
3. Final unstaged diff. Wait for `STAGE <lane>` before any `git add`.
4. Commit on the lane branch only after `STAGE`. Integrator fetches and merges.

## 3. Universal lane rules

1. One owner, one worktree, one branch, one frozen path set.
2. Stay Candidate / unsupported. Never promote. Never expose a public API
   whose transport authority does not exist.
3. Never `git add -A`, never touch another worktree, never edit
   `docs/rfcs/012-implementation-plan.md` or this runbook.
4. Native paths, IDs, prompts, content, and secrets never enter fixtures,
   Debug, logs, reports, or portable DTOs.
5. Authority is non-serializable and evidence-backed.
6. Preflight bounds before retaining attacker-sized input.
7. Report exact test pass counts. Run `git diff --check` before every handoff.
8. Stop and report rather than invent policy, authority, or path expansion.

## 4. Path ownership

| Lane | Owns | Must not touch |
| --- | --- | --- |
| C2 | Durable/scoped usage-v2 + actor-run/affiliation parity still required for Claude promotion; digest-bound reports under `agent-support/claude-code/candidate-*/reports/` | Candidate ADS/scope/support-release documents; catalog durability/schema; support authorization; RFC 012D public transport; promotion |
| A3 | Claude Candidate evidence under `agent-support/claude-code/candidate-*` (documents, hashes, conformance). Keep Candidate + unsupported | Runtime composition, catalog engine, observer admission, promotion, Codex/Grok packages |
| B2 | Claude catalog composition + source-access/coverage producer behind Candidate denial | Persistence/public catalog N-API; B3 schema; promotion; scoped D2 child admission; A3 documents |
| Integrator | Plan status, this runbook, delta ledger, `draft/`, merges, shared barrels | — |

Shared files (`Cargo.toml`, `index.d.ts`, crate `mod.rs` barrels) need prior
integrator approval even for a one-line compile fix.

## 5. Lane objectives

**C2.** Close the smallest remaining RFC 012C C2 gap that still blocks Claude
promotion: durable vs scoped entity identity, semantic revision, correction,
complete/partial replacement, retraction, actor, affiliation, and coverage
parity for already selected families (`runtime.usage-v2`, actor-run,
actor-affiliation). Do not change `getUsage` / query selection. Do not add a
new semantic family.

**A3.** Finish the candidate-only Claude evidence package: exact artifact pin,
deterministic sanitized transitions, complete claimed RFC 012D relation
coverage, required RFC 012C semantic fixtures, identity/compositionality/
cross-topology checks, bounded performance evidence inputs, and human
sanitizer-review inputs. Keep Candidate. Do not implement runtime composition.

**B2.** Implement the actual common/runtime Claude catalog composition plus
source-access/coverage producer behind Candidate denial. Conformance may
execute with synthetic authorization; the built-in Candidate must remain
impossible to authorize. Bind complete membership authority, exact access
policy/declaration/selection, component completion, source coverage, and final
identity parity. No persistence, public API, policy expansion, or D2 child
admission.

## 6. After Wave I

Integrator runs the combined matrix, then either `CLAUDE PROMOTION GATE PASSED`
or `CLAUDE PROMOTION BLOCKED`. Wave II is not auto-spawned.
