# RFC archive

Superseded program documents, kept because they are the record of how a
decision was reached — not because they describe the system. Nothing here is
current. If a statement in this directory disagrees with code, the code is
right.

Archived 2026-08-23 by the RFC 012 landing (lane L8; see
[../012-landing-plan.md](../012-landing-plan.md)).

## RFC 012 child contracts — full drafts

The current contracts are the trimmed files one directory up. These are the
2026-08-15 drafts they were trimmed from, including the sections the landing
replaced or deleted.

| Draft | Current contract |
| --- | --- |
| [012B catalog, readiness, progressive startup](./012b-catalog-readiness-and-progressive-startup-2026-08-15-draft.md) | [../012b-…](../012b-catalog-readiness-and-progressive-startup.md) |
| [012C runtime semantics and usage-v2](./012c-runtime-semantics-and-usage-v2-2026-08-15-draft.md) | [../012c-…](../012c-runtime-semantics-and-usage-v2.md) |
| [012D database-free session-scoped observation](./012d-session-scoped-observation-2026-08-15-draft.md) | [../012d-…](../012d-session-scoped-observation.md) |

## RFC 012 program plans and runbooks

Retired as execution authority by [../012-landing-plan.md](../012-landing-plan.md).

- [012-implementation-plan.md](./012-implementation-plan.md) — the 4,339-line
  gate-driven plan the landing replaced.
- [012-implementation-deduplication-plan.md](./012-implementation-deduplication-plan.md)
- [012-parallel-work-handoff.md](./012-parallel-work-handoff.md)
- [012-wave-i-execution.md](./012-wave-i-execution.md)
- [012-wave-iii-execution.md](./012-wave-iii-execution.md)

## RFC 012 evidence

The measurements that justified the architecture. Still true as measurements;
the conclusions drawn from them live in the current child RFCs.

- [012-phase-0-census-2026-08-15.md](./012-phase-0-census-2026-08-15.md) —
  catalog discoverability census.
- [012-runtime-observation-census-2026-08-15.md](./012-runtime-observation-census-2026-08-15.md) —
  the response-repeat evidence behind usage-v2.
- [012-rfc011-delta-evidence-audit-2026-08-17.md](./012-rfc011-delta-evidence-audit-2026-08-17.md)

The census scripts these reports were produced with are still in `scripts/`
(`catalog_census`, `runtime_observation_census`, `diagnostic_census`,
`team_affiliation_census`, `rfc012_experiments`); `package.json` and
`crates/spaghetti-napi` still reference them, so they were not archived.
