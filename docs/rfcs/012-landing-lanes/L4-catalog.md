# Lane L4 — catalog-first startup and the readiness vector (RFC 012B), simplified and wired

Worktree: `/Volumes/SamsungRed/spaghetti-rfc012/worktrees/land-l4-catalog`, branch `land/l4-catalog`.
Read `COMMON.md` first.

## Consumer and outcome

The playground and CLI (`spag projects`, `spag sessions`, `spag doctor`) still
list projects/sessions through the RFC 011 history path and only after full
convergence. The Phase 0 census proved the whole catalog (176 projects /
1,414 sessions) is discoverable from 0.83% of transcript bytes. Outcome:
projects and sessions appear (complete or explicitly degraded) seconds after
engine open; history, usage, artifacts, and FTS converge in the background; a
single readiness vector tells the UI/`doctor` what is complete; stable
`ExternalEntityRef`s (VibeField Phase A) identify every catalog row. The 012B
epoch/publication/lineage machinery (~40k lines, zero product callers) is
replaced by a design proportionate to one SQLite file on one machine.

## Facts

- Two catalog paths: `list_history_projects/sessions` (011, used by
  `packages/cli/src/commands/projects.ts:45 api.getProjectList()` →
  `observation-service.ts:227`) vs `list_library_projects_json/sessions_json`
  (012B, zero product callers). `engine/progressive_startup.rs:5-7`:
  "retains the legacy history path when catalog authority is not yet promoted".
- 012B stack: `catalog_contract.rs` (2.3k) + `catalog_contract/` (18.3k incl.
  6.7k tests), `engine/catalog_state.rs` (12k; `decode_stored_state` is 1,265
  lines), `engine/catalog_publication.rs` (5.4k), `engine/catalog_{query,build,refresh,hydration,retention}`,
  `engine/catalog_query/` (4.8k), `source/catalog_composition.rs`, per-adapter
  `claude|codex|grok/catalog_runtime.rs` + `catalog_conformance.rs` (+ support
  probes). 225 `Catalog*` types; twelve `*Expectation` types; five readiness
  surfaces (`EngineStatusSnapshot`+`LifecyclePhase`, `ProgressiveHostReadiness`,
  `CatalogReadinessSnapshot/Phase/Machine`, `ProjectionReadiness`,
  `RuntimeUsageV2ProjectionReadiness`). Only the first two are consumed by
  `observation-host.ts`.
- The two-phase bootstrap already exists and runs (`startConfiguredObservation`
  → `completeQueryBootstrap`, 08-13): search is unavailable until query
  bootstrap completes. Build on it.
- `ExternalEntityRef`/`session_key`/`project_key` derivation is RFC 012A
  (ratified) — find where it is derived today (`adapter/` or
  `catalog_contract`) and keep that derivation byte-for-byte; it is what
  VibeField will persist.
- Schema is drop-and-rebuild (`core/schema.rs` v62); 89 → 111 tables since
  08-14 — expect to delete most catalog tables you replace.

## Target design (proportionate)

1. **Discovery pass on open**: for each configured source (Claude/Codex/Grok
   via their `AgentAdapter`), enumerate catalog evidence cheaply (bounded head
   records / index files, as the census did) and upsert catalog rows
   (`projects`, `sessions` with `external_ref`, native ids, project
   association evidence + provenance, `catalog_state ∈ {discovered,
   transcript_backed, hydrated, searchable}`, `degraded` flag + reason) in one
   transaction per source; then publish `catalog: ready|degraded`. Warm start:
   the last committed catalog rows are served immediately (SQLite gives the
   snapshot); a rescan reconciles by mtime/size.
2. **Readiness vector**: one struct `Readiness { catalog, history, usage,
   capabilities, artifacts, search }`, each `{ state, committed_at_seq,
   detail }`, derived from committed rows — not a state machine with epochs,
   coverage plans, and anchors. It replaces the five surfaces (collapse or
   delegate; L3 owns the usage field's meaning). Exposed via one napi method +
   `ts-rs` type; consumed by `observation-host.ts`, `spag doctor`, and the
   playground's library screen (a small "indexing…" indicator is enough).
3. **Queries**: `listProjects/listSessions` (existing names) return catalog
   rows with `catalog_state` and `at_commit_seq`; keyset pagination stays
   stable because reads are snapshot-consistent; identity conflicts are
   returned explicitly (RFC 012B rule), not merged. Delete the parallel
   `list_library_*`/page/snapshot/hydration request/response stacks, or make
   them the only path — there must be one.
4. **Hydration/priority**: "promote selected session" is an explicit command
   on the writer/scheduler (keep if it exists and is small; otherwise a
   follow-up), never a query side effect.
5. **Delete**: the 012B machinery above, the per-adapter catalog
   runtime/conformance triplicates (keep the minimal per-adapter discovery
   code as adapter methods), `rfc012b*.ts` hand-written contracts and the SDK
   client functions nobody calls (coordinate: L2 leaves rfc012b-* to you).

## Proof and performance

- Behavioral tests on temp source trees (real adapter layouts, fixture JSONL
  the decoder accepts): cold catalog before any history row; warm start serves
  last catalog instantly; degraded marking when a source errors; rescan after
  mtime change; identity conflict surfaces; `ExternalEntityRef` stable across
  restarts.
- Measure on the real corpus (`~/.claude/projects`, read-only): time from
  engine open → catalog ready (cold and warm); report the numbers (plan §6
  budgets: warm < 1 s, cold < 10 s). Bench gate scripts stay green.
- CLI: `spag projects` / `spag sessions` work during background ingest and
  show the readiness state; `spag doctor` prints the vector.

## Out of scope

Usage semantics (L3), observer (L1), SDK codegen/barrel (L2), adapter decoder
internals.
