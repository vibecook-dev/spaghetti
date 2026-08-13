# RFC 011 Phases 0–8 audit and remediation ledger

Status: repository implementation and verification complete; external private-corpus acceptance remains; last reconciled 2026-08-12

This is the durable Phase 0–8 closure record for RFCs 010 and 011. It audits
the implementation as a system rather than treating an isolated unit test as a
phase exit. Every finding stays here after it is fixed so the reason for the
change and its regression evidence are not lost.

Status vocabulary:

- **closed** — implementation and focused regression evidence exist;
- **verifying** — implementation exists but the final repository-wide gate has
  not completed since the last change;
- **open** — required implementation or retirement work remains;
- **external acceptance** — the repository provides the mechanism, but the
  accepted private corpus or release decision must come from a maintainer.

## Executive finding

The native observation architecture is now real rather than Claude-specific:
the common coordinator is registry-driven, Claude, Codex, and Grok all
implement `AgentAdapter`, one production-shaped host can supervise all three in
one Rust-owned database, and canonical cold/live/reconcile/restart fixture
differentials are zero for all three.

Phase 8 production ownership is now cut over. The SDK package entry exposes the
async Rust-owned observation service and canonical client; CLI commands and the
TUI await that service; the playground utility process owns one Rust host and
main/renderer processes use framed clients. The retired TypeScript engine is
reachable only through a repository-relative test barrel that is absent from
package exports. The old Rust bulk/live writer is behind a non-default
`legacy-oracle` feature and absent from generated production bindings.

The architecture gate now models the shipped TypeScript graph and default Rust
module graph. Every production ownership allowlist is empty: zero TypeScript
SQLite authorities/drivers, query projection mutators, source-local runtime
services, common-Rust built-in dispatch sites, direct-engine consumer reads,
portable-client leaks, playground owner bypasses, host import leaks, or default
legacy-oracle exports.

The contract gaps found by this audit are now implemented and covered by the
vendor-neutral `adapter-check` gate: durable SDK-visible capability manifests,
the RFC retention vocabulary and safe defaults, bounded `SourceAccess` with
dependency revisions, all six declared source drivers, adapter panic
boundaries, restrictive local file permissions, and deterministic source
identity independent of unrelated registration order. Every repository-owned
Phase 0–8 remediation and verification gate is complete. The only remaining
exit item is a maintainer's acceptance of a private-corpus differential; the
runner is implemented, content-safe, and opt-in, but acceptance is not an
implementation decision this audit can make on the maintainer's behalf.

## Findings ledger

### Roadmap ownership and boundaries

1. **Phases 6–8 originally had no implementation or exit evidence — closed.**
   Codex and Grok implement the same `AgentAdapter` contract as Claude; generic
   N-API lifecycle methods and `openObservationHost` compose all three; SDK,
   CLI, and playground select that owner in production.

2. **The adapter registry was not the runtime composition root — closed.**
   `AdapterRegistry` now resolves open adapter IDs; the N-API composition root
   registers Claude, Codex, and Grok; common engine modules no longer construct
   a concrete built-in adapter.

3. **The architecture ratchet missed concrete or constructed adapter coupling
   — closed.** The checker now scans Rust syntax/dependencies rather than only
   quoted source-ID literals and rejects adapter/storage boundary violations.
   Finding 30 closes the production-entry transitive graph boundary as well.

### Common runtime integration

4. **The bounded scheduler was declared but unused — closed.** Object work is
   admitted through the priority scheduler and decoded on a bounded Rayon pool;
   saturation, fairness, cancellation, and maximum-in-flight behavior have
   focused tests.

5. **Discovery repeated a whole-root walk for every stream — closed.** One
   confined physical-root inventory is shared across selectors during a
   reconcile.

6. **`DirectorySnapshot` was rejected by the production coordinator —
   closed.** Directory membership is now reconciled and committed through the
   common catalog path with add/remove/change coverage.

7. **Append batching was disabled — closed.** Backfill/catch-up commits up to
   64 complete records per transaction while preserving per-record provenance
   and atomic decoder state.

8. **An unchanged poll wrote `last_seen_at` — closed.** Reservation is skipped
   when catalog identity and adapter contract already match; an idle refresh
   leaves the commit watermark unchanged.

9. **Native watcher startup had no polling-only fallback — closed.** Watcher
   construction or registration failure enters adaptive polling and later
   recovery without blocking observation startup.

10. **Retry cadence was adapter-wide rather than object-scoped — closed.**
    Incomplete append tails retain a precise instance/stream/object target and
    the supervisor retries that object without rescanning unrelated roots.

### Correctness, confinement, and lifecycle

11. **One matching invalid object could abort an adapter — closed.** A
    record-permanent bootstrap failure quarantines that object path and other
    objects/streams continue.

12. **Stable malformed-revision quarantine was only an isolated primitive —
    closed.** Replace/snapshot retry state is durable and becomes quarantine
    after a bounded number of unchanged-revision failures, including restart.

13. **Path discovery had a symlink time-of-check/time-of-use race — closed.**
    common readers use confined component-by-component, no-follow opens and
    reject path escapes even after discovery.

14. **Observation cancellation was not cooperative — closed.** Cancellation
    reaches discovery, scheduling, read/decode, query, and pre-commit checks;
    shutdown no longer waits behind an unbounded queued command.

15. **The old ingest differential tested the legacy row writer, not RFC 011
    observation — closed.** `scripts/observation-diff.ts` exercises the native
    coordinator and canonical tables for cold build, live append, forced
    reconcile, generation replacement, and restart.

16. **Claude had fixture evidence but no accepted private-corpus record —
    external acceptance.** The deterministic differential runner now accepts
    explicit roots and all fixture sizes. Small and medium public Claude
    corpora pass; a maintainer still needs to run and accept the private corpus
    without checking its contents into the repository.

17. **Change-log retention and typed `ResetRequired` were absent — closed.**
    age/size/minimum-window pruning, durable floor metrics, stale-cursor reset,
    and client transport mapping are implemented and tested.

18. **Usage facts declared `Delta`, `Cumulative`, and `Snapshot`, but the
    projector rejected the latter two — closed.** Durable series state now
    performs snapshot replacement, cumulative differencing, reset epochs, and
    immediate-successor repair for late counters in O(changed facts). Quality
    reclassification cannot double count.

19. **A valid append batch ending in recognized ignored telemetry failed
    cursor validation — closed.** The commit cursor may end after the final
    emitted fact when trailing records are `IgnoredKnown`; each emitted fact's
    provenance remains range-validated.

20. **Schema changes silently broke positional query fixtures — closed.**
    A full Rust sweep found stale `source_objects` and `usage_contributions`
    inserts after the retry-state and usage-series additions. Fixtures now name
    columns explicitly so future additive columns do not invalidate unrelated
    query tests. The default production graph passes 318 tests; the superset
    repository oracle graph passes 503 tests with `legacy-oracle` enabled.

### Adapter contract and conformance gaps

21. **No vendor-neutral executable `adapter-check` report existed — closed.**
    `scripts/adapter-check.ts` selects one/all built-ins, runs the common
    source/transaction/coordinator/watch/capability packs, native decoder
    goldens, canonical convergence traces, and the SDK host/manifest pack, then
    emits one versioned JSON report. The all-adapter gate passes 13/13 packs.

22. **Raw/error retention did not match the RFC privacy contract — closed.**
    Retention now uses `None`, `HashOnly`, `DiagnosticExcerpt`, and `Full`.
    Diagnostic excerpts are structurally redacted and byte bounded, sensitive
    built-in streams default to non-duplicating policies, and fixtures cover
    secret-like keys and values.

23. **Capability manifests were not surfaced through the SDK — closed.** The
    complete adapter version, source schema versions, support level,
    granularity, availability, and notes snapshot is persisted with each source
    instance and returned by typed Rust, N-API, client, and IPC `listSources`
    queries even while source roots are offline.

24. **The trait lacked bounded `SourceAccess` and stamped dependency reads —
    closed.** Adapters receive confined `read_object`, named
    `query_source_db`, and bounded `list_objects` capabilities. File, query,
    and directory-membership dependencies are stamped and revalidated before
    commit so a changed dependency dirties the decode instead of publishing a
    mixed revision.

25. **Fresh-build entity/source identities depended on unrelated insertion
    order — closed.** Catalog source namespaces are deterministic BLAKE3
    derivations of `(adapter_id, stable_key)`, constrained to JavaScript-safe
    positive integers. Reversed multi-adapter registration produces identical
    source and canonical entity identities.

26. **Adapter panics were not converted at the common boundary — closed.** All
    adapter discovery, declaration, bootstrap, and decode calls cross a
    controlled unwind boundary. A panic becomes a scoped adapter failure and
    no partial reservation/fact transaction is published.

27. **Restrictive database/owner-sidecar permissions were not enforced —
    closed.** The database, WAL/SHM, owner lock, and owner metadata are created
    or tightened to owner-only Unix permissions with regression coverage. The
    current framed IPC host uses transferred `MessagePort`s and creates no
    filesystem endpoint; any future filesystem transport remains required to
    apply the same helper at creation.

28. **Phase 8 production ownership had not cut over — closed.** The SDK root,
    CLI, and playground now use `createObservationService`; all reads traverse
    `SpaghettiClient`. Legacy TypeScript readers/watchers/parsers/checkpoints,
    projection writers, and the live sequence bus remain repository-only behind
    `legacy-oracle.ts`, which is not a package export. The old Rust bulk/live
    bridge is a non-default feature and is absent from production N-API types.

29. **The differential was narrower than the mandatory conformance packs —
    closed.** The executable gate composes driver, coordinator, supervisor,
    transaction, projection/capability, decoder-golden, SDK-host, and canonical
    convergence suites. Together they cover partial/malformed/unknown records,
    dropped/duplicated hints, delete/recreate, cancellation/disposal, restart,
    and every declared driver/capability for the built-ins.

30. **The production-shaped host lacked its own transitive zero-debt
    architecture rule — closed.** The architecture checker walks the
    `observation-host.ts` runtime import graph and rejects TypeScript source
    readers, watchers, parsers, SQLite/schema, and projection writers.

31. **Two RFC-required v1 source drivers were absent — closed.** Read-only,
    query-only `SqliteSnapshot` and `KeyValueSnapshot` drivers now implement
    bounded rows/values/snapshot bytes, schema/data-version watermarks,
    cancellation/busy retry, deterministic ordering/checkpoints, and complete
    replacement semantics. Synthetic conformance adapters prove changed and
    deleted rows retract atomically through the common coordinator.

32. **Engine-selection compatibility surfaces could revive or advertise a
    second production owner — closed.** `resolveEngine()` and active-engine
    diagnostics are Rust-only, legacy environment/config switches are ignored,
    `spag engine ts` returns a retirement error, and the playground neither
    reads nor forwards an engine selector. Missing native binaries block
    startup rather than silently falling back.

33. **The manual SDK native interface still advertised legacy bulk/live N-API
    methods after the default Rust feature gate — closed.** Production
    `NativeAddon` now contains only the version and persistent engine opener.
    The old methods live in `legacy-native.ts`, require an explicitly
    feature-built addon, and are reachable only from the repository oracle.

34. **The cut-over product facade lacked a multi-adapter regression at its
    actual public boundary — closed.** The SDK integration fixture initializes
    all three adapters through `createObservationService`, asserts source-neutral
    message DTOs, native-session scoped search, unknown-project isolation,
    failed-start cleanup, and successful ownership reacquisition.

35. **The real-corpus schema validators and compatibility DTOs had drifted
    from current Claude Code output — closed.** Current data added frame links,
    fallback content blocks, refusal/agent-killed system events, tool names,
    attribution/feedback fields, prompt-suggestion settings, telemetry fields,
    active-session key sidecars, issue-cache files, and subagent metadata. The
    types now model those additions without exposing bridge key material; the
    cache parser preserves unknown issue records; and validator-owned copies of
    TypeScript field sets were replaced where possible. A 200-session,
    353,742-message private sample passes all 10 session/message checks, all 11
    secondary-data checks, and the complete config/settings validator.

36. **TypeScript script entrypoints depended on the `tsx` CLI's local IPC
    socket — closed.** Restricted hosts could reject that socket with `EPERM`
    before a differential or benchmark started. Root ingest, observation, and
    benchmark commands now invoke the same loader with `node --import tsx`,
    which needs no control socket; the medium differential command passes
    unchanged through the public wrapper.

37. **The SDK suite mixed hermetic tests with machine-specific acceptance
    probes — closed.** The private-corpus integration now requires
    `SPAGHETTI_RUN_PRIVATE_INTEGRATION=1`; native FSEvents-only assertions skip
    when that backend is unavailable while the resource-bounded polling and
    watcher-loss recovery suites still run. This prevents ordinary CI from
    hanging on a developer corpus or failing merely because a native watcher
    backend cannot start.

38. **A CLI isolation test wrote a sentinel into the developer's real Claude
    home — closed.** The suite now snapshots any pre-existing sentinel and
    performs all writes under temporary fake homes. All 110 CLI tests pass and
    the real `~/.claude` remains byte-identical.

39. **The production SDK build still packaged a repository-only parser worker
    path — closed.** Worker generation was removed from the production build,
    watcher/parser packages are development-only, and a packaging test asserts
    that neither `parse-worker.js` nor an inlined replacement appears in
    `dist`.

40. **Default feature-gating stranded the Phase 0 legacy oracle commands —
    closed.** Ingest differentials and the historical ingest benchmark now
    build `legacy-oracle` into a unique temporary output directory, scope
    `NAPI_RS_NATIVE_LIBRARY_PATH` to the oracle subprocess, and remove the
    directory afterward. A small-fixture cold differential reports zero diffs;
    hashes of the generated production declaration and addon are unchanged by
    the run.

## Phase assessment

| Phase | Current assessment | Remaining exit evidence |
| --- | --- | --- |
| 0 | Complete baseline and freeze; zero-debt production graph enforced | None. |
| 1 | Persistent engine lifecycle complete | None. |
| 2 | Transaction/outbox/retention core complete | None. |
| 3 | All six source drivers and bounded dependency access integrated | None. |
| 4 | Claude history/usage implementation complete on public fixtures | Accepted private-corpus differential remains a maintainer gate. |
| 5 | Claude capability packs, native supervisor, manifests, and conformance complete | None. |
| 6 | Codex adapter, public differential, conformance, and production cutover complete | None. |
| 7 | Grok adapter, dependency contract, public differential, and production cutover complete | None. |
| 8 | Complete: one Rust production owner; all architecture allowlists are zero | None. |

## Remediation plan

Work proceeds in dependency order:

1. ~~Keep this ledger synchronized and restore a green Rust-suite baseline.~~
2. ~~Close deterministic identity, panic isolation, retention/privacy,
   durable capability manifests, and dependency reads.~~
3. ~~Add the missing read-only source drivers and vendor-neutral
   `adapter-check`.~~
4. ~~Add the zero-debt native observation-host import graph.~~
5. ~~Migrate first-party production owners to that host, isolate all legacy
   TypeScript/Rust oracle code, and shrink the Phase 8 allowlists to zero.~~
6. ~~Run Rust unit/integration/clippy/format, SDK typecheck/tests, all adapter
   differentials, adapter conformance, architecture checks, leak/cancellation
   tests, and performance smoke gates.~~
7. record accepted private-corpus evidence separately without retaining private
   source contents.

## Verification evidence

Final verification on 2026-08-12 produced the following repository evidence:

- `cargo test -p spaghetti-napi --lib --quiet`: 318/318 default-production
  tests pass;
- the same command with `--features legacy-oracle`: 503/503 repository-oracle
  tests pass;
- strict `cargo clippy ... --all-targets -- -D warnings` passes in both default
  and `legacy-oracle` configurations, and `cargo fmt --check` passes;
- `pnpm adapter-check`: 13/13 common and adapter packs pass for Claude, Codex,
  and Grok;
- `pnpm test:observation-diff:medium`: exact canonical tables and cursors across
  cold, live, reconcile, generation replacement, and restart for 32 sessions,
  714 messages, and 47 durable cursors;
- the small Claude, Codex, and Grok versions of that same differential pass as
  part of `adapter-check`;
- `pnpm test:ingest-diff` still reports zero legacy-row diffs through the
  isolated temporary feature build without changing production bindings;
- `pnpm test:query-conformance`: all 12 query groups pass, including the bounded
  12 MiB payload probe;
- `pnpm bench:queries --runs 5 --warmup 1`: conformance remains exact, 100/100
  queued cancellations reject with a successful recovery query, and ten
  concurrent readers remain correct during a live commit;
- the SDK suite passes 311 tests with seven native-FSEvents-only cases skipped
  on this host; the repository-only legacy-native smoke and private-corpus
  acceptance suites are intentionally opt-in;
- the CLI suite passes 110/110 and the playground integration set passes 21/21;
- `pnpm typecheck`, `pnpm lint`, `pnpm format:check`, `pnpm build`, and
  `pnpm validate` pass. Validation reports 4/4 suites, including 10/10
  session/message checks, 11/11 secondary checks, and all-empty RFC 011
  architecture allowlists;
- the generated default N-API declaration exposes the persistent observation
  engine but no legacy bulk/live writer, production SDK bundles contain no
  parser worker or TypeScript SQLite owner, and `git diff --check` passes.

The full private corpus is 3.6 GiB on the verification host. It was sampled for
schema coverage, but no Phase 4 acceptance differential is claimed here: that
run copies and indexes the corpus in isolated temporary storage and must be
explicitly requested and accepted by a maintainer. Its JSON report contains
only counts and SHA-256 digests, never transcript content.

## Completion gates

Phases 0–8 are closed only when:

- every non-external finding above is marked closed with a regression test;
- all three built-in adapters pass the same explicit core/driver/capability
  command and their canonical fresh/live/reconcile/restart projections match;
- the production database has exactly one enrolled Rust owner;
- no production TypeScript module reads agent-owned content for ingest or
  writes an ingest projection;
- TypeScript source watcher/parser/checkpoint/write-hook/live-sequence
  allowlists and common Rust built-in dispatch are zero;
- capability and retention behavior are visible and truthful through the SDK;
- shutdown leaves no watcher, worker, database, callback, or socket behind;
- the full repository verification matrix is green after the final change.
